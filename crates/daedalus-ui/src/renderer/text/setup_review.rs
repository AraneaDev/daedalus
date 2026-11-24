use super::{AppState, ChromeLineSpec, Rect, TextRenderer, Theme, truncate_right_with_ellipsis};
use crate::setup_review::{Control, modal_rect, region, role_label};
use unicode_width::UnicodeWidthStr;

impl TextRenderer {
    pub(super) fn build_setup_review_items(
        &mut self,
        state: &AppState,
        theme: &Theme,
        w: f32,
        h: f32,
    ) {
        let review = &state.setup_review;
        let dpi = self.scale_factor;
        let modal = modal_rect(w, h, dpi);
        let scale = dpi.min(modal.width / 400.).min(modal.height / 405.);
        let glyph = self.cell_w / (self.font_size_logical * dpi);
        let mut line = |text: &str, rect: Rect, size: f32, color, bold, centered| {
            let inset = 6. * scale;
            let max_w = (rect.width - 2. * inset).max(1.);
            let columns = (max_w / (glyph * size * scale)) as usize;
            let label = truncate_right_with_ellipsis(text, columns);
            let spec = ChromeLineSpec {
                text: &label,
                bounds: rect,
                left: rect.x + inset,
                top: rect.y + (rect.height - size * 1.4 * scale) / 2.,
                max_w,
                font_size_logical: size * scale / dpi,
                line_height_logical: size * 1.4 * scale / dpi,
                color,
                bold,
            };
            if centered {
                self.push_modal_button(spec);
            } else {
                self.push_modal_line(spec);
            }
        };
        line(
            "Review environment",
            region(modal, 0.035, 0.012, 0.93, 0.072),
            super::MODAL_TITLE_FONT_SIZE_LOGICAL,
            theme.text_primary,
            true,
            false,
        );
        line(
            &format!("{}  ·  {}", review.name, review.host()),
            region(modal, 0.035, 0.084, 0.93, 0.052),
            super::CHROME_SUB_FONT_SIZE_LOGICAL,
            theme.text_muted,
            false,
            false,
        );
        for i in 0..3 {
            let status = review.status(i);
            let y = 0.165 + i as f32 * 0.135;
            line(
                role_label(i),
                region(modal, 0.035, y, 0.39, 0.056),
                14.,
                theme.text_primary,
                true,
                false,
            );
            let attention = matches!(status, "Missing" | "Needs check" | "Incomplete command");
            line(
                status,
                region(modal, 0.43, y, 0.29, 0.056),
                12.,
                if attention {
                    theme.status_busy
                } else {
                    theme.text_muted
                },
                attention,
                false,
            );
        }
        for (control, rect) in review.controls(modal) {
            let active = review.focus == control;
            if control == Control::Tool && !review.busy && !review.enabled(control) {
                line(
                    &format!("Missing tool · {}", review.label(control)),
                    rect,
                    super::CHROME_FONT_SIZE_LOGICAL,
                    theme.text_primary,
                    true,
                    false,
                );
                continue;
            }
            let text = match control {
                Control::Command(i) if active => {
                    let max = ((rect.width - 12. * scale)
                        / (glyph * super::CHROME_FONT_SIZE_LOGICAL * scale))
                        as usize;
                    let value = review.command(i);
                    let before = super::truncate_left_with_ellipsis(
                        &value[..review.caret()],
                        max.saturating_sub(1),
                    );
                    let after = truncate_right_with_ellipsis(
                        &value[review.caret()..],
                        max.saturating_sub(before.width() + 1),
                    );
                    format!("{before}│{after}")
                }
                Control::Command(i) if review.command(i).is_empty() => "Add command…".into(),
                _ => review.label(control),
            };
            let color = if !review.enabled(control) {
                theme.text_muted
            } else if control == Control::Open {
                theme.bg_window
            } else if active {
                theme.border_focused
            } else {
                theme.text_primary
            };
            line(
                &text,
                rect,
                super::CHROME_FONT_SIZE_LOGICAL,
                color,
                active || control == Control::Open,
                !matches!(control, Control::Command(_) | Control::Dependencies),
            );
        }
        line(
            &review.detail(),
            region(modal, 0.035, 0.687, 0.93, 0.057),
            12.,
            theme.text_muted,
            false,
            false,
        );
        line(
            &review.feedback,
            region(modal, 0.035, 0.806, 0.93, 0.067),
            12.,
            theme.status_busy,
            false,
            false,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::renderer::tests::make_state_with_agent_row;
    use crate::setup_review::SetupAction;
    use daedalus_proto::{PaneRole, SetupInspection};

    #[test]
    fn windows_review_displays_launch_arguments_and_routes_clicks_at_rendered_bounds() {
        let (device, queue) = wgpu::Device::noop(&Default::default());
        let mut renderer = TextRenderer::new(
            &device,
            &queue,
            wgpu::TextureFormat::Bgra8UnormSrgb,
            1200,
            800,
            1.,
            14.,
            None,
        );
        let (mut state, layout) = make_state_with_agent_row();
        let executable = r"C:\Dev Tools\agent.exe";
        let argv = vec![executable.into(), r"C:\My Work\".into()];
        state.setup_review.begin(
            "Orbit".into(),
            r"C:\My Work".into(),
            SetupInspection {
                host: "win-dev".into(),
                platform: "windows".into(),
                commands: vec![(PaneRole::Agent, argv.clone())],
                available: vec![executable.into()],
                dependency_install: None,
            },
        );
        let frame = |renderer: &mut TextRenderer, state: &mut AppState| {
            renderer.prepare(&device, &queue, state, &layout, &Theme::dark(), 1200., 800.);
        };
        frame(&mut renderer, &mut state);
        let line = |text| {
            renderer
                .modal_items
                .iter()
                .find(|item| item.buffer.lines[0].text() == text)
                .unwrap()
        };
        line(r#""C:\Dev Tools\agent.exe" "C:\My Work\\""#);
        assert_eq!(
            state.setup_review.commands().unwrap(),
            [(PaneRole::Agent, argv, true)]
        );

        let click = |state: &mut AppState, label| {
            let b = line(label).bounds;
            state.setup_review.click(
                (b.left + b.right) as f32 / 2.,
                (b.top + b.bottom) as f32 / 2.,
                1200.,
                800.,
                1.,
            )
        };
        assert_eq!(click(&mut state, "Open workspace"), SetupAction::Open);
        assert_eq!(click(&mut state, "Back"), SetupAction::Back);
        state.setup_review.open = false;
        state
            .workspace_form
            .revisit(&state.setup_review.folder, &state.setup_review.name, true);
        frame(&mut renderer, &mut state);
        let labels: Vec<_> = renderer
            .modal_items
            .iter()
            .map(|item| item.buffer.lines[0].text())
            .collect();
        assert!(labels.contains(&"Set up workspace · Choose project"));
        assert!(labels.iter().any(|text| text.contains(r"C:\My Work")));
        assert!(!labels.contains(&"Open workspace"));
        assert!(state.workspace_form.name_is_fixed());
    }
}
