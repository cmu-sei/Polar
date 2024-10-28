# This is a gross hack. Home-manager has options that aren't valid for the
# system-wide nixos configuration. As a result I have to source the plug-ins
# directly to get them to load. Further, this needs to happen as soon as
# possible in the interactive shell initialization because these will override
# _my_ overrides (below).
source /.plugins.fish

function display_fzf_files --description="Call fzf and preview file contents using bat."
    set preview_command "bat --theme=gruvbox-dark --color=always --style=header,grid --line-range :400 {}"
    fzf --ansi --preview $preview_command
end

function display_rg_piped_fzf --description="Pipe ripgrep output into fzf"
    rg . -n --glob "!.git/" | fzf
end

function export --description="Emulates the bash export command"
    if [ $argv ] 
        set var (echo $argv | cut -f1 -d=)
        set val (echo $argv | cut -f2 -d=)
        set -gx $var $val
    else
        echo 'export var=value'
    end
end

function fd_fzf --description="Pipe fd output to fzf"
    set fd_exists (which fd)
    if test -z "$fd_exists"
        return
    end
    if test (is_valid_dir $argv) = "true"
        set go_to (fd -t d . $argv | fzf)
        if test -z "$go_to"
            return
        else
            pushd $go_to
            return
        end
    else
        echo "Must provide a valid search path."
    end
end

function filename_get_random --description="Sometimes you need a random name for a file and UUIDs suck"
    pwgen --capitalize --numerals --ambiguous 16 1
end

function files_compare --description="Requires two file paths to compare."
    if test $argv[1] = "" -o $argv[2] = ""
        echo "Arguments required for two files. Exiting."
        return 1
    end
    if test (sha512sum $argv[1] | cut -d ' ' -f 1) = (sha512sum $argv[2] | cut -d ' ' -f 1)
        return 0
    else
        return 1
    end
end

function files_compare_verbose --description="Text output for files_compare"
    if files_compare $argv[1] $argv[2]
        echo "Hashes match."
        return 0
    else
        echo "Hashes do not match."
        return 1
    end
end

function fish_greeting --description="Displays the Fish logo and some other init stuff."
    set_color $fish_color_autosuggestion
    set_color normal
    echo 'The license for this container can be found in /root/license.txt' | lolcat --force | cat
    lol_fig "Welcome to the Polar Shell."
    
    # Array of funny phrases
    set phrases "Brace yourself for a flurry of brilliant code!" "Our code is cooler than an ice cube in Antarctica!" "Get ready to chill and code!" "Let's make some code that's ice-olated in perfection!" "There are no polar bears, only coding bears!" "Next stop: Bug-free code!" "Where the only thing frozen is the bugs!" "It's time to break the ice and dive into development!" "Where every line of code is as cool as the Arctic!" "Let's code like penguins on ice!" "Navigating the icy waters of code with ease!" "Our coding skills are as sharp as an icicle!" "Chill vibes, hot code!" "Frosty fingers, fiery code!" "Coding in a winter wonderland!" "Sliding into smooth code like a penguin!" "Our code is a polar express to success!" "Ice-cold focus, blazing fast code!" "From the tundra to triumph with our code!" "Arctic-level precision in every line!" "Coding through the polar vortex of bugs!" "Snow problem we can't solve with code!" "Where coding brilliance is as vast as the polar ice cap!" "Cool minds, warm hearts, perfect code!"

    # Select a random funny phrase
    set random_index (random 1 (count $phrases))
    set phrase $phrases[$random_index]

    echo $phrase | lolcat --force | cat
end

function hash_get --description="Return a hash of the input string."
    echo -n $argv[1] | sha1sum | cut -d ' ' -f 1
end

function json_validate --description="Validate provided json against provided schema. E.g., 'validate_json file.json schema.json'. Can also handle json input from pipe via stdin."
    # jsonschema only operates via file input, which is inconvenient.
    if ! isatty stdin
        set -l tmp_file get_random_filename
        read > /tmp/$tmp_file; or exit -1
        jsonschema -F "{error.message}" -i /tmp/$tmp_file $argv[1]
        rm -f /tmp/$tmp_file
    else
        jsonschema -F "{error.message}" -i $argv[1] $argv[2]
    end
end

function lol_fig --description="lolcat inside a figlet"
    echo $argv | figlet | lolcat -f | cat
end

#function lh --description="ls -alh, so you don't have to."
    #grc ls -alh --color $argv
#end
function lh --description="Uses eza, a replacement for ls, with some useful options as defaults."
    eza --group --header --group-directories-first --long --icons --git --all --binary --dereference --links $argv
end

function myps --description="ps auww --ppid 2 -p2 --deselect"
    ps auww --ppid 2 -p2 --deselect
end

function nvim_goto_files --description="Open fzf to find a file, then open it in neovim"
    set nvim_exists (which nvim)
    if test -z "$nvim_exists"
        return
    end

    set selection (display_fzf_files)
    if test -z "$selection"
        return
    else
        nvim $selection
    end
end

function nvim_goto_line --description="ripgrep to find contents, search results using fzf, open selected result in neovim, on the appropriate line."
    set nvim_exists (which nvim)
    if test -z "$nvim_exists"
        return
    end

    set selection (display_rg_piped_fzf)
    if test -z "$selection"
        return
    else 
        set filename (echo $selection | awk -F ':' '{print $1}')
        set line (echo $selection | awk -F ':' '{print $2}')
        nvim +$line $filename
    end
end

function is_valid_dir --description="Checks if the argument passed is a valid directory path"
    if test (is_valid_argument $argv) = "true" -a (path_exists $argv) = "true" -a (is_a_directory $argv) = "true"
        echo "true"
    else
        echo "false"
    end
end

function is_valid_argument --description="Checks if it has been passed a valid argument"
    # Is there a valid argument?
    if test (count $argv) -gt 0
        echo "true"
    else
        echo "false"
    end
end

function path_exists --description="Checks if the path exists"
    # Does it exist?
    if test -e $argv[1]
        echo "true"
    else
        echo "false"
    end
end

function is_a_directory --description="Checks if the path is a directory"
    # Is it a directory?
    if test -d $argv[1]
        echo "true"
    else
        echo "false"
    end
end

function keyring_unlock --description="unlocks the gnome keyring from the shell"
    read -s -P "Password: " pass
    for m in (echo -n $pass | gnome-keyring-daemon --replace --unlock)
        export $m
    end
    set -e pass
end

function var_erase --description="Fish shell is missing a proper ability to delete a var from all scopes."
    set -el $argv[1]
    set -eg $argv[1]
    set -eU $argv[1]
end

function yaml_to_json --description="Converts YAML input to JSON output."
    python -c 'import sys, yaml, json; y=yaml.safe_load(sys.stdin.read()); print(json.dumps(y))' $argv[1] | read; or exit -1
end

function code_server --description="Starts code-server in the background."
    # Create log directory if it doesn't exist.
    if not test -d /tmp/log
        mkdir -p /tmp/log
    end
    # Create a log file for the code-server instance.
    set -l log_file /tmp/log/code-server-$(date +%s).log
    # Run code-server in the background and redirect output to the log file.
    code-server --auth none --bind-addr 0.0.0.0:8080 --disable-telemetry --disable-update-check --disable-workspace-trust /workspace > $log_file 2>&1 &
    echo "code-server started on port 8080. Log file: $log_file"
end

# You'll want to install some nerd fonts, patched for powerline support of the theme.
# Recommend: 'UbuntuMono Nerd Font 13'
# gsettings set org.gnome.desktop.interface monospace-font-name 'UbuntuMono Nerd Font 13'
set -gx theme_nerd_fonts yes

# bobthefish is the theme of choice. This setting chooses a default color scheme.
#set -g theme_color_scheme gruvbox

# Gruvbox Color Palette
set -l foreground ebdbb2
set -l selection 282828 
set -l comment 928374 
set -l red fb4934
set -l orange fe8019
set -l yellow fabd2f
set -l green b8bb26
set -l cyan 8ec07c
set -l blue 83a598
set -l purple d3869b

# Syntax Highlighting Colors
set -g fish_color_normal $foreground
set -g fish_color_command $cyan
set -g fish_color_keyword $blue
set -g fish_color_quote $yellow
set -g fish_color_redirection $foreground
set -g fish_color_end $orange
set -g fish_color_error $red
set -g fish_color_param $purple
set -g fish_color_comment $comment
set -g fish_color_selection --background=$selection
set -g fish_color_search_match --background=$selection
set -g fish_color_operator $green
set -g fish_color_escape $blue
set -g fish_color_autosuggestion $comment

# Completion Pager Colors
set -g fish_pager_color_progress $comment
set -g fish_pager_color_prefix $cyan
set -g fish_pager_color_completion $foreground
set -g fish_pager_color_description $comment

set -gx theme_color_scheme gruvbox

set -gx theme_display_vi yes
set -gx theme_display_sudo_user yes
set -gx theme_show_exit_status yes
set -gx theme_display_jobs_verbose yes

# Vi key bindings
set -gx fish_key_bindings fish_vi_key_bindings

# The following plug-ins help us to:
#  - execute non-native scripts and capture their shell state for Fish
#  - Execute 'doas' efficiently, adding it when we may have forgot
#set -gx fish_plugins bass grc foreign-env bobthefish fzf fzf-fish 

set -gx LS_COLORS 'rs=0:di=00;34:ln=00;36:mh=00:pi=40;33:so=00;35:do=00;35:bd=40;33;00:cd=40;33;00:or=40;31;00:mi=00:su=37;41:sg=30;43:ca=30;41:tw=30;42:ow=34;42:st=37;44:ex=38;5;196:*.yaml=38;5;226:*.yml=38;5;226:*.json=38;5;226:*.csv=38;5;226:*.tar=38;5;207:*.tgz=38;5;207:*.arc=38;5;207:*.arj=38;5;207:*.taz=38;5;207:*.lha=38;5;207:*.lz4=38;5;207:*.lzh=38;5;207:*.lzma=38;5;207:*.tlz=38;5;207:*.txz=38;5;207:*.tzo=38;5;207:*.t7z=38;5;207:*.zip=38;5;207:*.z=38;5;207:*.dz=38;5;207:*.gz=38;5;207:*.lrz=38;5;207:*.lz=38;5;207:*.lzo=38;5;207:*.xz=38;5;207:*.zst=38;5;207:*.tzst=38;5;207:*.bz2=38;5;207:*.bz=38;5;207:*.tbz=38;5;207:*.tbz2=38;5;207:*.tz=38;5;207:*.deb=38;5;207:*.rpm=38;5;207:*.jar=38;5;207:*.war=38;5;207:*.ear=38;5;207:*.sar=38;5;207:*.rar=38;5;207:*.alz=38;5;207:*.ace=38;5;207:*.zoo=38;5;207:*.cpio=38;5;207:*.7z=38;5;207:*.rz=38;5;207:*.cab=38;5;207:*.wim=38;5;207:*.swm=38;5;207:*.dwm=38;5;207:*.esd=38;5;207:*.jpg=00;35:*.jpeg=00;35:*.mjpg=00;35:*.mjpeg=00;35:*.gif=00;35:*.bmp=00;35:*.pbm=00;35:*.pgm=00;35:*.ppm=00;35:*.tga=00;35:*.xbm=00;35:*.xpm=00;35:*.tif=00;35:*.tiff=00;35:*.png=00;35:*.svg=00;35:*.svgz=00;35:*.mng=00;35:*.pcx=00;35:*.mov=00;35:*.mpg=00;35:*.mpeg=00;35:*.m2v=00;35:*.mkv=00;35:*.webm=00;35:*.webp=00;35:*.ogm=00;35:*.mp4=00;35:*.m4v=00;35:*.mp4v=00;35:*.vob=00;35:*.qt=00;35:*.nuv=00;35:*.wmv=00;35:*.asf=00;35:*.rm=00;35:*.rmvb=00;35:*.flc=00;35:*.avi=00;35:*.fli=00;35:*.flv=00;35:*.gl=00;35:*.dl=00;35:*.xcf=00;35:*.xwd=00;35:*.yuv=00;35:*.cgm=00;35:*.emf=00;35:*.ogv=00;35:*.ogx=00;35:*.aac=00;36:*.au=00;36:*.flac=00;36:*.m4a=00;36:*.mid=00;36:*.midi=00;36:*.mka=00;36:*.mp3=00;36:*.mpc=00;36:*.ogg=00;36:*.ra=00;36:*.wav=00;36:*.oga=00;36:*.opus=00;36:*.spx=00;36:*.xspf=00;36:'

set -gx EZA_COLORS '*.tar=38;5;203:*.tgz=38;5;203:*.arc=38;5;203:*.arj=38;5;203:*.taz=38;5;203:*.lha=38;5;203:*.lz4=38;5;203:*.lzh=38;5;203:*.lzma=38;5;203:*.tlz=38;5;203:*.txz=38;5;203:*.tzo=38;5;203:*.t7z=38;5;203:*.zip=38;5;203:*.z=38;5;203:*.dz=38;5;203:*.gz=38;5;203:*.lrz=38;5;203:*.lz=38;5;203:*.lzo=38;5;203:*.xz=38;5;203:*.zst=38;5;203:*.tzst=38;5;203:*.bz2=38;5;203:*.bz=38;5;203:*.tbz=38;5;203:*.tbz2=38;5;203:*.tz=38;5;203:*.deb=38;5;203:*.rpm=38;5;203:*.jar=38;5;203:*.war=38;5;203:*.ear=38;5;203:*.sar=38;5;203:*.rar=38;5;203:*.alz=38;5;203:*.ace=38;5;203:*.zoo=38;5;203:*.cpio=38;5;203:*.7z=38;5;203:*.rz=38;5;203:*.cab=38;5;203:*.wim=38;5;203:*.swm=38;5;203:*.dwm=38;5;203:*.esd=38;5;203:*.doc=38;5;109:*.docx=38;5;109:*.pdf=38;5;109:*.txt=38;5;109:*.md=38;5;109:*.rtf=38;5;109:*.odt=38;5;109:*.yaml=38;5;172:*.yml=38;5;172:*.json=38;5;172:*.toml=38;5;172:*.conf=38;5;172:*.config=38;5;172:*.ini=38;5;172:*.env=38;5;172:*.jpg=38;5;132:*.jpeg=38;5;132:*.png=38;5;132:*.gif=38;5;132:*.bmp=38;5;132:*.tiff=38;5;132:*.svg=38;5;132:*.mp3=38;5;72:*.wav=38;5;72:*.aac=38;5;72:*.flac=38;5;72:*.ogg=38;5;72:*.m4a=38;5;72:*.mp4=38;5;72:*.avi=38;5;72:*.mov=38;5;72:*.mkv=38;5;72:*.flv=38;5;72:*.wmv=38;5;72:*.c=38;5;142:*.cpp=38;5;142:*.py=38;5;142:*.java=38;5;142:*.js=38;5;142:*.ts=38;5;142:*.go=38;5;142:*.rs=38;5;142:*.php=38;5;142:*.html=38;5;142:*.css=38;5;142::*.nix=38;5;142:*.rs=38;5;142di=38;5;109:ur=38;5;223:uw=38;5;203:ux=38;5;142:ue=38;5;142:gr=38;5;223:gw=38;5;203:gx=38;5;142:tr=38;5;223:tw=38;5;203:tx=38;5;142:su=38;5;208:sf=38;5;208:xa=38;5;108:nb=38;5;244:nk=38;5;108:nm=38;5;172:ng=38;5;208:nt=38;5;203:ub=38;5;244:uk=38;5;108:um=38;5;172:ug=38;5;208:ut=38;5;203:lc=38;5;208:lm=38;5;208:uu=38;5;223:gu=38;5;223:un=38;5;223:gn=38;5;223:da=38;5;109:ga=38;5;108:gm=38;5;109:gd=38;5;203:gv=38;5;142:gt=38;5;108:gi=38;5;244:gc=38;5;203:Gm=38;5;108:Go=38;5;172:Gc=38;5;142:Gd=38;5;203:xx=38;5;237'

if set -q KITTY_INSTALLATION_DIR
    set --global KITTY_SHELL_INTEGRATION enabled no-sudo
    source "$KITTY_INSTALLATION_DIR/shell-integration/fish/vendor_conf.d/kitty-shell-integration.fish"
    set --prepend fish_complete_path "$KITTY_INSTALLATION_DIR/shell-integration/fish/vendor_completions.d"
end

# This version uses 'fd', instead of find.
set -xg FZF_CTRL_T_COMMAND "fd --type file --hidden 2>/dev/null | sed 's#^\./##'"

set -xg BAT_THEME gruvbox-dark

set -xg MANPAGER "sh -c 'col -bx | bat -l man -p'"

set -xg FZF_DEFAULT_OPTS '--prompt="🔭 " --height 80% --layout=reverse --border'

set -xg FZF_DEFAULT_COMMAND 'rg --files --no-ignore --hidden --follow --glob "!.git/"'

set -xg BAT_THEME gruvbox-dark

set -xg EDITOR /nix/store/yiz4s56j80q3c22psybvnib2vmkrpgw3-neovim-0.9.5/bin/nvim

#set -xg VIM $CURRENT_USER_HOME/.config/nvim

# This is safer for special shells than kitty.
set -xg TERM screen-256color

set -xg NIXOS_OZONE_WL 1

# Aliases:
function nvimf
    nvim_goto_files $argv
end

function nviml
    nvim_goto_line $argv
end

function fdfz
    fd_fzf $argv
end

function rgk
    rg --hyperlink-format=kitty $argv
end


# add completions generated by NixOS to $fish_complete_path
begin
  # joins with null byte to accommodate all characters in paths, then
  # respectively gets all paths before (exclusive) / after (inclusive) the
  # first one including "generated_completions",

  # splits by null byte, and then removes all empty lines produced by using
  # 'string'
  set -l prev (string join0 $fish_complete_path | \
      string match --regex "^.*?(?=\x00[^\x00]*generated_completions.*)" | \
      string split0 | string match -er ".")

  set -l post (string join0 $fish_complete_path | \
      string match --regex "[^\x00]*generated_completions.*" | \
      string split0 | string match -er ".")

  set fish_complete_path $prev "/etc/fish/generated_completions" $post
end
# prevent fish from generating completions on first run
if not test -d $__fish_user_data_dir/generated_completions
  $COREUTILS/bin/mkdir \
      $__fish_user_data_dir/generated_completions
end

if test -n "$__NIXOS_SET_ENVIRONMENT_DONE"
    set -el PATH
    set -eg PATH
    set -eU PATH
end