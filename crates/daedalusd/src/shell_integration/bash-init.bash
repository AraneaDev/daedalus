# shellcheck shell=bash
# Daedalus non-destructive bash shell integration (OSC 7 + OSC 133)
if [ -r /etc/profile ]; then
    # shellcheck source=/dev/null
    . /etc/profile
fi
if [ -r "$HOME/.bashrc" ]; then
    # shellcheck source=/dev/null
    . "$HOME/.bashrc"
fi

__daedalus_urlencode() {
    local str="$1" i c hex
    local LC_ALL=C
    for (( i=0; i<${#str}; i++ )); do
        c="${str:i:1}"
        case "$c" in
            [a-zA-Z0-9.~_/-]) printf '%s' "$c" ;;
            *) printf -v hex '%%%02X' "'$c"; printf '%s' "$hex" ;;
        esac
    done
}

__daedalus_in_cmd=1
__daedalus_first_prompt=1

__daedalus_preexec() {
    case "$BASH_COMMAND" in
        __daedalus_precmd_start|__daedalus_precmd_end) return ;;
    esac
    if [ "$__daedalus_in_cmd" -eq 0 ]; then
        __daedalus_in_cmd=1
        printf '\033]133;C\007'
    fi
}

__daedalus_precmd_start() {
    local exit_code=$?
    __daedalus_in_cmd=1
    if [ "$__daedalus_first_prompt" -eq 0 ]; then
        printf '\033]133;D;%d\007' "$exit_code"
    fi
    __daedalus_first_prompt=0
    local encoded_pwd
    encoded_pwd="$(__daedalus_urlencode "$PWD")"
    printf '\033]7;file://%s%s\007' "${HOSTNAME:-localhost}" "$encoded_pwd"
    return "$exit_code"
}

__daedalus_precmd_end() {
    __daedalus_in_cmd=0
}

if [[ "$PS1" != *"133;A"* ]]; then
    PS1="\[\033]133;A\007\]${PS1}\[\033]133;B\007\]"
fi

if [[ "$(declare -p PROMPT_COMMAND 2>/dev/null)" == "declare -a"* ]]; then
    if (( ${BASH_VERSINFO[0]:-0} > 5 || (${BASH_VERSINFO[0]:-0} == 5 && ${BASH_VERSINFO[1]:-0} >= 1) )); then
        PROMPT_COMMAND=("__daedalus_precmd_start" "${PROMPT_COMMAND[@]}" "__daedalus_precmd_end")
    else
        __daedalus_joined_pc="__daedalus_precmd_start"
        for __daedalus_pc_item in "${PROMPT_COMMAND[@]}"; do
            if [ -n "${__daedalus_pc_item}" ]; then
                __daedalus_joined_pc+=$'\n'"${__daedalus_pc_item}"
            fi
        done
        __daedalus_joined_pc+=$'\n__daedalus_precmd_end'
        unset PROMPT_COMMAND
        # Bash before 5.1 needs a scalar PROMPT_COMMAND after unset.
        # shellcheck disable=SC2178
        PROMPT_COMMAND="${__daedalus_joined_pc}"
        unset __daedalus_joined_pc __daedalus_pc_item
    fi
elif [ -n "${PROMPT_COMMAND:-}" ]; then
    PROMPT_COMMAND=$'__daedalus_precmd_start\n'"${PROMPT_COMMAND}"$'\n__daedalus_precmd_end'
else
    PROMPT_COMMAND=$'__daedalus_precmd_start\n__daedalus_precmd_end'
fi
trap '__daedalus_preexec' DEBUG
