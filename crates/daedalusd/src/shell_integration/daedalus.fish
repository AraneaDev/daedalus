# Daedalus non-destructive fish shell integration (OSC 7 + OSC 133)
status is-interactive; or return

if not set -q __daedalus_fish_initialized
    set -g __daedalus_fish_initialized 1

    function __daedalus_preexec --on-event fish_preexec
        printf '\033]133;C\007'
    end

    function __daedalus_postexec --on-event fish_postexec
        set -l exit_code $status
        printf '\033]133;D;%d\007' $exit_code
    end

    function __daedalus_prompt --on-event fish_prompt
        set -l encoded_pwd (string replace -a -- '%' '%25' $PWD | string replace -a -- ' ' '%20')
        set -l host $hostname
        if test -z "$host"
            set host localhost
        end
        printf '\033]7;file://%s%s\007' "$host" "$encoded_pwd"
        printf '\033]133;A\007'

        if functions -q fish_prompt; and not functions -q __daedalus_orig_fish_prompt
            functions -c fish_prompt __daedalus_orig_fish_prompt
            function fish_prompt
                __daedalus_orig_fish_prompt
                printf '\033]133;B\007'
            end
        end
    end
end
