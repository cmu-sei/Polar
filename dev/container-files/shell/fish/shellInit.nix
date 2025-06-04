{ # fish/shellInit.nix
  pkgs, ...
}:
''
# Guard to avoid re-sourcing
if not set -q __fish_nixos_shell_config_sourced
    set -g __fish_nixos_shell_config_sourced 1

    if not contains /etc/fish/vendor_functions.d $fish_function_path
        set --prepend fish_function_path /etc/fish/vendor_functions.d
    end

    # Universal user paths and env vars

    # Determine the current user's home, even when elevated via doas/sudo
    if set -q DOAS_USER
        set -gx CURRENT_USER_HOME /home/$DOAS_USER
    else
        set -gx CURRENT_USER_HOME $HOME
    end

    # For scripting code automation tasks.
    set -gx CODE_ROOT $CURRENT_USER_HOME/Documents/projects/codes

end

# Conditionally source login shell config
status is-login; and not set -q __fish_nixos_login_config_sourced
and begin
    source /etc/fish/interactiveShellInit.fish
    set -g __fish_nixos_login_config_sourced 1
end
''
