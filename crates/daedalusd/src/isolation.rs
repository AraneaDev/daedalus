mod compose;
mod dotenv;
mod ports;

pub use compose::run_compose_down;
use compose::{
    ParsedPortEntry, find_compose_file, parse_compose_ports, sanitize_compose_project,
    write_compose_override,
};
use dotenv::{rewrite_localhost_url_port, scan_env_file};
use ports::allocate_port_offset;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::io::{self, ErrorKind};
use std::path::{Path, PathBuf};

/// Result of computing port/Compose/env isolation for a workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IsolationPlan {
    pub slot_index: u16,
    pub port_offset: u16,
    pub compose_project_name: Option<String>,
    pub compose_override_path: Option<PathBuf>,
    pub injected_env: HashMap<String, String>,
    pub remapped_ports: HashMap<u16, u16>,
}

/// Computes a conflict-free port offset, rewrites `.env` port/URL variables,
/// and generates `.daedalus/compose.override.yml` when a Compose file exists.
pub fn prepare_workspace_isolation(
    name: &str,
    root_dir: &Path,
    slot_index: u16,
    caller_env: &HashMap<String, String>,
) -> io::Result<IsolationPlan> {
    prepare_workspace_isolation_with_reserved(
        name,
        root_dir,
        slot_index,
        caller_env,
        &HashSet::new(),
    )
}

/// Computes a conflict-free port offset while treating `reserved_ports` as unavailable,
/// rewrites `.env` port/URL variables, and generates `.daedalus/compose.override.yml`
/// when a Compose file exists.
pub fn prepare_workspace_isolation_with_reserved(
    name: &str,
    root_dir: &Path,
    slot_index: u16,
    caller_env: &HashMap<String, String>,
    reserved_ports: &HashSet<u16>,
) -> io::Result<IsolationPlan> {
    prepare_workspace_isolation_with_extra_ports(
        name,
        root_dir,
        slot_index,
        caller_env,
        reserved_ports,
        &[],
    )
}

/// Computes a conflict-free port offset while treating `reserved_ports` as unavailable
/// and including `extra_ports` (e.g. from `.daedalus.toml` `[ports].extra`) in
/// port reservation and offset remapping.
pub fn prepare_workspace_isolation_with_extra_ports(
    name: &str,
    root_dir: &Path,
    slot_index: u16,
    caller_env: &HashMap<String, String>,
    reserved_ports: &HashSet<u16>,
    extra_ports: &[u16],
) -> io::Result<IsolationPlan> {
    let env_scan = scan_env_file(root_dir)?;
    let mut env_lookup = env_scan.all_vars.clone();
    for (k, v) in caller_env {
        env_lookup.insert(k.clone(), v.clone());
    }

    let compose_file = find_compose_file(root_dir);
    let compose_services = match &compose_file {
        Some(path) => parse_compose_ports(path, &env_lookup)?,
        None => BTreeMap::new(),
    };

    let mut base_ports = BTreeSet::new();
    for entries in compose_services.values() {
        for entry in entries {
            if let ParsedPortEntry::Remappable { host_port, .. } = entry {
                base_ports.insert(*host_port);
            }
        }
    }
    for (_, port) in &env_scan.port_vars {
        base_ports.insert(*port);
    }
    for (_, _, port) in &env_scan.url_vars {
        base_ports.insert(*port);
    }
    for &port in extra_ports {
        if port > 0 {
            base_ports.insert(port);
        }
    }

    let port_offset = allocate_port_offset(slot_index, &base_ports, reserved_ports)?;

    let mut remapped_ports = HashMap::new();
    if port_offset > 0 {
        for &base in &base_ports {
            let mapped = base.checked_add(port_offset).ok_or_else(|| {
                io::Error::new(
                    ErrorKind::InvalidInput,
                    format!("port {base} + offset {port_offset} overflows u16"),
                )
            })?;
            remapped_ports.insert(base, mapped);
        }
    }

    let mut injected_env = HashMap::new();
    let (compose_project_name, compose_override_path) = match compose_file {
        Some(base_path) => {
            let project = sanitize_compose_project(name, slot_index);
            let override_path = write_compose_override(root_dir, &compose_services, port_offset)?;
            let sep = if cfg!(windows) { ";" } else { ":" };
            injected_env.insert("COMPOSE_PROJECT_NAME".to_string(), project.clone());
            injected_env.insert(
                "COMPOSE_FILE".to_string(),
                format!("{}{sep}{}", base_path.display(), override_path.display()),
            );
            (Some(project), Some(override_path))
        }
        None => (None, None),
    };

    if port_offset > 0 {
        for (key, port) in &env_scan.port_vars {
            if let Some(mapped) = port.checked_add(port_offset) {
                injected_env.insert(key.clone(), mapped.to_string());
            }
        }
        for (key, raw_url, _) in &env_scan.url_vars {
            if let Some(rewritten) = rewrite_localhost_url_port(raw_url, port_offset) {
                injected_env.insert(key.clone(), rewritten);
            }
        }
    }

    for (k, v) in caller_env {
        injected_env.insert(k.clone(), v.clone());
    }

    Ok(IsolationPlan {
        slot_index,
        port_offset,
        compose_project_name,
        compose_override_path,
        injected_env,
        remapped_ports,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use noyalib::compat::serde_yaml::{self, Value};
    use std::net::TcpListener;
    use tempfile::tempdir;

    #[test]
    fn parses_short_and_long_compose_ports() {
        let dir = tempdir().unwrap();
        let compose = dir.path().join("compose.yml");
        std::fs::write(
            &compose,
            r#"
services:
  web:
    image: node:20
    ports:
      - "8080:80"
      - "127.0.0.1:5432:5432"
      - "3000:3000/tcp"
      - "${WEB_PORT:-4000}:4000"
      - "9090"
  db:
    image: postgres:16
    ports:
      - target: 5432
        published: 15432
        protocol: tcp
        host_ip: 127.0.0.1
"#,
        )
        .unwrap();

        let env = HashMap::new();
        let parsed = parse_compose_ports(&compose, &env).unwrap();

        let web = parsed.get("web").unwrap();
        assert_eq!(web.len(), 5);
        assert!(matches!(
            &web[0],
            ParsedPortEntry::Remappable {
                host_ip: None,
                host_port: 8080,
                container_port,
                protocol: None,
            } if container_port == "80"
        ));
        assert!(matches!(
            &web[1],
            ParsedPortEntry::Remappable {
                host_ip: Some(ip),
                host_port: 5432,
                container_port,
                protocol: None,
            } if ip == "127.0.0.1" && container_port == "5432"
        ));
        assert!(matches!(
            &web[2],
            ParsedPortEntry::Remappable {
                host_ip: None,
                host_port: 3000,
                container_port,
                protocol: Some(proto),
            } if container_port == "3000" && proto == "tcp"
        ));
        assert!(matches!(
            &web[3],
            ParsedPortEntry::Remappable {
                host_ip: None,
                host_port: 4000,
                container_port,
                protocol: None,
            } if container_port == "4000"
        ));
        assert!(matches!(&web[4], ParsedPortEntry::Passthrough(_)));

        let db = parsed.get("db").unwrap();
        assert_eq!(db.len(), 1);
        assert!(matches!(
            &db[0],
            ParsedPortEntry::Remappable {
                host_ip: Some(ip),
                host_port: 15432,
                container_port,
                protocol: Some(proto),
            } if ip == "127.0.0.1" && container_port == "5432" && proto == "tcp"
        ));
    }

    #[test]
    fn rewrites_dotenv_ports_and_localhost_urls() {
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join(".env"),
            r#"
# App settings
PORT=3400
export API_PORT="8480"
DATABASE_URL=postgres://user:p@ss@localhost:5732/app?sslmode=disable
REDIS_URL='redis://127.0.0.1:6479/0'
REMOTE_URL=https://api.example.com:8443/v1
LOCAL_NO_PORT_URL=http://localhost/health
"#,
        )
        .unwrap();

        let mut caller_env = HashMap::new();
        caller_env.insert("API_PORT".to_string(), "9999".to_string());

        let plan = prepare_workspace_isolation("My App", dir.path(), 1, &caller_env).unwrap();

        assert_eq!(plan.slot_index, 1);
        assert_eq!(plan.port_offset, 100);
        assert_eq!(plan.compose_project_name, None);
        assert_eq!(plan.compose_override_path, None);

        assert_eq!(
            plan.injected_env.get("PORT").map(String::as_str),
            Some("3500")
        );
        // Caller env overrides remapped .env variable.
        assert_eq!(
            plan.injected_env.get("API_PORT").map(String::as_str),
            Some("9999")
        );
        assert_eq!(
            plan.injected_env.get("DATABASE_URL").map(String::as_str),
            Some("postgres://user:p@ss@localhost:5832/app?sslmode=disable")
        );
        assert_eq!(
            plan.injected_env.get("REDIS_URL").map(String::as_str),
            Some("redis://127.0.0.1:6579/0")
        );
        assert!(!plan.injected_env.contains_key("REMOTE_URL"));
        assert!(!plan.injected_env.contains_key("LOCAL_NO_PORT_URL"));

        assert_eq!(plan.remapped_ports.get(&3400), Some(&3500));
        assert_eq!(plan.remapped_ports.get(&5732), Some(&5832));
        assert_eq!(plan.remapped_ports.get(&6479), Some(&6579));
    }

    #[test]
    fn falls_back_to_env_example_and_skips_remap_for_slot_zero() {
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join(".env.example"),
            "PORT=4500\nDATABASE_URL=postgres://localhost:5432/db\n",
        )
        .unwrap();

        let plan = prepare_workspace_isolation("primary", dir.path(), 0, &HashMap::new()).unwrap();

        assert_eq!(plan.port_offset, 0);
        assert!(plan.remapped_ports.is_empty());
        assert!(!plan.injected_env.contains_key("PORT"));
        assert!(!plan.injected_env.contains_key("DATABASE_URL"));
    }

    #[test]
    fn bumps_port_block_on_real_tcp_conflict() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join(".env"), "PORT=3000\n").unwrap();

        let blocker = TcpListener::bind(("127.0.0.1", 3100))
            .expect("port 3100 must be free before blocking it for conflict test");

        let plan =
            prepare_workspace_isolation("wt-conflict", dir.path(), 1, &HashMap::new()).unwrap();

        drop(blocker);

        assert_eq!(plan.slot_index, 1);
        assert_eq!(plan.port_offset, 200);
        assert_eq!(plan.remapped_ports.get(&3000), Some(&3200));
        assert_eq!(
            plan.injected_env.get("PORT").map(String::as_str),
            Some("3200")
        );
    }

    #[test]
    fn skips_reserved_ports_from_other_workspaces() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join(".env"), "PORT=3000\n").unwrap();

        let reserved = HashSet::from([3200]);
        let plan = prepare_workspace_isolation_with_reserved(
            "wt-reserved",
            dir.path(),
            2,
            &HashMap::new(),
            &reserved,
        )
        .unwrap();

        assert_eq!(plan.slot_index, 2);
        assert_eq!(plan.port_offset, 300);
        assert_eq!(plan.remapped_ports.get(&3000), Some(&3300));
        assert_eq!(
            plan.injected_env.get("PORT").map(String::as_str),
            Some("3300")
        );
    }

    #[test]
    fn generates_compose_override_with_override_tag_and_env() {
        let dir = tempdir().unwrap();
        let compose_path = dir.path().join("docker-compose.yaml");
        std::fs::write(
            &compose_path,
            r#"
services:
  web:
    ports:
      - "${WEB_PORT:-4200}:3000/tcp"
      - "127.0.0.1:8280:80"
      - "9090"
  db:
    ports:
      - target: 5432
        published: 5632
        host_ip: 127.0.0.1
        protocol: tcp
"#,
        )
        .unwrap();

        let plan = prepare_workspace_isolation("My Repo!", dir.path(), 1, &HashMap::new()).unwrap();

        let override_path = plan
            .compose_override_path
            .as_ref()
            .expect("override path must be set");
        assert!(override_path.is_file());
        assert_eq!(
            override_path,
            &dir.path().join(".daedalus").join("compose.override.yml")
        );
        assert_eq!(plan.compose_project_name.as_deref(), Some("my-repo-wt1"));
        assert_eq!(
            plan.injected_env
                .get("COMPOSE_PROJECT_NAME")
                .map(String::as_str),
            Some("my-repo-wt1")
        );
        let sep = if cfg!(windows) { ";" } else { ":" };
        assert_eq!(
            plan.injected_env.get("COMPOSE_FILE").map(String::as_str),
            Some(format!("{}{sep}{}", compose_path.display(), override_path.display()).as_str())
        );

        let yaml_text = std::fs::read_to_string(override_path).unwrap();
        assert!(yaml_text.contains("!override"), "yaml was:\n{yaml_text}");
        assert!(
            yaml_text.contains("4300:3000/tcp"),
            "yaml was:\n{yaml_text}"
        );
        assert!(
            yaml_text.contains("127.0.0.1:8380:80"),
            "yaml was:\n{yaml_text}"
        );
        assert!(yaml_text.contains("9090"), "yaml was:\n{yaml_text}");
        assert!(
            yaml_text.contains("127.0.0.1:5732:5432/tcp"),
            "yaml was:\n{yaml_text}"
        );

        // Verify the YAML parses back with `!override` tags intact.
        let parsed: Value = serde_yaml::from_str(&yaml_text).unwrap();
        let web_ports = &parsed["services"]["web"]["ports"];
        assert!(matches!(
            web_ports,
            Value::Tagged(tagged) if tagged.tag().nobang() == "override"
        ));
    }

    #[test]
    fn resolves_bare_env_var_in_compose_and_handles_ipv6_short_syntax() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join(".env"), "APP_PORT=4600\n").unwrap();
        std::fs::write(
            dir.path().join("compose.yml"),
            r#"
services:
  app:
    ports:
      - "${APP_PORT}:8080"
      - "[::1]:5932:5432"
"#,
        )
        .unwrap();

        let plan = prepare_workspace_isolation("app", dir.path(), 1, &HashMap::new()).unwrap();
        assert_eq!(plan.remapped_ports.get(&4600), Some(&4700));
        assert_eq!(plan.remapped_ports.get(&5932), Some(&6032));

        let yaml_text =
            std::fs::read_to_string(plan.compose_override_path.as_ref().unwrap()).unwrap();
        assert!(yaml_text.contains("4700:8080"), "yaml was:\n{yaml_text}");
        assert!(
            yaml_text.contains("[::1]:6032:5432"),
            "yaml was:\n{yaml_text}"
        );
    }

    #[test]
    fn errors_when_port_offset_overflows_u16() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join(".env"), "PORT=65500\n").unwrap();

        let err = prepare_workspace_isolation("overflow", dir.path(), 1, &HashMap::new())
            .expect_err("65500 + 100 should overflow u16 across all probe blocks");
        assert_eq!(err.kind(), ErrorKind::AddrInUse);
    }

    #[test]
    fn sanitize_compose_project_matches_spec_examples() {
        assert_eq!(sanitize_compose_project("myrepo", 0), "myrepo");
        assert_eq!(sanitize_compose_project("myrepo", 1), "myrepo-wt1");
        assert_eq!(sanitize_compose_project("---", 0), "daedalus");
        assert_eq!(
            sanitize_compose_project("Feat/Auth_V2", 3),
            "feat-auth_v2-wt3"
        );
    }

    #[test]
    fn extra_ports_are_remapped_and_avoid_reserved_and_bound_conflicts() {
        let dir = tempdir().unwrap();
        let blocker = TcpListener::bind(("127.0.0.1", 19100))
            .expect("port 19100 must be free before blocking");
        let reserved = HashSet::from([19280]);

        let plan = prepare_workspace_isolation_with_extra_ports(
            "extra",
            dir.path(),
            1,
            &HashMap::new(),
            &reserved,
            &[0, 19000, 19080],
        )
        .unwrap();
        drop(blocker);

        // Slot 1 starts at offset 100 (19100 blocked by TCP listener),
        // bumps to offset 200 (19280 blocked by reserved_ports),
        // and lands on offset 300.
        assert_eq!(plan.port_offset, 300);
        assert_eq!(plan.remapped_ports.get(&19000), Some(&19300));
        assert_eq!(plan.remapped_ports.get(&19080), Some(&19380));
        assert!(!plan.remapped_ports.contains_key(&0));
    }
}
