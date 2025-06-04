{
  fisheyGrc,
  bass,
  bobthefish,
  starshipBin,
  atuinBin,
  editor,
  fishShell,
}:

''
# Only source once per interactive session
if status is-interactive; and not set -q __fish_nixos_interactive_config_sourced
    set -g __fish_nixos_interactive_config_sourced 1

    # TODO: make sure this is still correct. This is used for updatedb, I seem to recall.
    set -gx PRUNEPATHS /dev /proc /sys /media /mnt /lost+found /nix /sys /tmp

    # This is a gross hack. Home-manager has options that aren't valid for the
    # system-wide nixos configuration. As a result I have to source the plug-ins
    # directly to get them to load. Further, this needs to happen as soon as
    # possible in the interactive shell initialization because these will override
    # GRC
    source ${fisheyGrc}/share/fish/vendor_conf.d/grc.fish
    source ${fisheyGrc}/share/fish/vendor_functions.d/grc.wrap.fish

    # Bass
    source ${bass}/share/fish/vendor_functions.d/bass.fish

    # Bobthefish (must load BEFORE you override colors/theme)
    source ${bobthefish}/share/fish/vendor_functions.d/__bobthefish_glyphs.fish
    source ${bobthefish}/share/fish/vendor_functions.d/fish_mode_prompt.fish
    source ${bobthefish}/share/fish/vendor_functions.d/fish_right_prompt.fish
    source ${bobthefish}/share/fish/vendor_functions.d/__bobthefish_colors.fish
    source ${bobthefish}/share/fish/vendor_functions.d/fish_title.fish
    source ${bobthefish}/share/fish/vendor_functions.d/__bobthefish_display_colors.fish
    source ${bobthefish}/share/fish/vendor_functions.d/fish_prompt.fish
    source ${bobthefish}/share/fish/vendor_functions.d/bobthefish_display_colors.fish

    # Atuin
    ${atuinBin} init fish | source

    # Starship
    source (${starshipBin} init fish --print-full-init | psub)

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

    # Theme configuration (bobthefish)

    # You'll want to install some nerd fonts, patched for powerline support of the theme.
    # Recommend: 'UbuntuMono Nerd Font 13'
    set -gx theme_nerd_fonts yes

    # bobthefish is the theme of choice. This setting chooses a default color scheme.
    set -gx theme_color_scheme gruvbox

    set -gx theme_display_vi yes
    set -gx theme_display_sudo_user yes
    set -gx theme_show_exit_status yes
    set -gx theme_display_jobs_verbose yes

    # Vi key bindings
    set -gx fish_key_bindings fish_vi_key_bindings

    set -gx LS_COLORS 'rs=0:di=00;34:ln=00;36:mh=00:pi=40;33:so=00;35:do=00;35:bd=40;33;00:cd=40;33;00:or=40;31;00:mi=00:su=37;41:sg=30;43:ca=30;41:tw=30;42:ow=34;42:st=37;44:ex=38;5;196:*.yaml=38;5;226:*.yml=38;5;226:*.json=38;5;226:*.csv=38;5;226:*.tar=38;5;207:*.tgz=38;5;207:*.arc=38;5;207:*.arj=38;5;207:*.taz=38;5;207:*.lha=38;5;207:*.lz4=38;5;207:*.lzh=38;5;207:*.lzma=38;5;207:*.tlz=38;5;207:*.txz=38;5;207:*.tzo=38;5;207:*.t7z=38;5;207:*.zip=38;5;207:*.z=38;5;207:*.dz=38;5;207:*.gz=38;5;207:*.lrz=38;5;207:*.lz=38;5;207:*.lzo=38;5;207:*.xz=38;5;207:*.zst=38;5;207:*.tzst=38;5;207:*.bz2=38;5;207:*.bz=38;5;207:*.tbz=38;5;207:*.tbz2=38;5;207:*.tz=38;5;207:*.deb=38;5;207:*.rpm=38;5;207:*.jar=38;5;207:*.war=38;5;207:*.ear=38;5;207:*.sar=38;5;207:*.rar=38;5;207:*.alz=38;5;207:*.ace=38;5;207:*.zoo=38;5;207:*.cpio=38;5;207:*.7z=38;5;207:*.rz=38;5;207:*.cab=38;5;207:*.wim=38;5;207:*.swm=38;5;207:*.dwm=38;5;207:*.esd=38;5;207:*.jpg=00;35:*.jpeg=00;35:*.mjpg=00;35:*.mjpeg=00;35:*.gif=00;35:*.bmp=00;35:*.pbm=00;35:*.pgm=00;35:*.ppm=00;35:*.tga=00;35:*.xbm=00;35:*.xpm=00;35:*.tif=00;35:*.tiff=00;35:*.png=00;35:*.svg=00;35:*.svgz=00;35:*.mng=00;35:*.pcx=00;35:*.mov=00;35:*.mpg=00;35:*.mpeg=00;35:*.m2v=00;35:*.mkv=00;35:*.webm=00;35:*.webp=00;35:*.ogm=00;35:*.mp4=00;35:*.m4v=00;35:*.mp4v=00;35:*.vob=00;35:*.qt=00;35:*.nuv=00;35:*.wmv=00;35:*.asf=00;35:*.rm=00;35:*.rmvb=00;35:*.flc=00;35:*.avi=00;35:*.fli=00;35:*.flv=00;35:*.gl=00;35:*.dl=00;35:*.xcf=00;35:*.xwd=00;35:*.yuv=00;35:*.cgm=00;35:*.emf=00;35:*.ogv=00;35:*.ogx=00;35:*.aac=00;36:*.au=00;36:*.flac=00;36:*.m4a=00;36:*.mid=00;36:*.midi=00;36:*.mka=00;36:*.mp3=00;36:*.mpc=00;36:*.ogg=00;36:*.ra=00;36:*.wav=00;36:*.oga=00;36:*.opus=00;36:*.spx=00;36:*.xspf=00;36:'

    set -gx EZA_COLORS '*.tar=38;5;203:*.tgz=38;5;203:*.arc=38;5;203:*.arj=38;5;203:*.taz=38;5;203:*.lha=38;5;203:*.lz4=38;5;203:*.lzh=38;5;203:*.lzma=38;5;203:*.tlz=38;5;203:*.txz=38;5;203:*.tzo=38;5;203:*.t7z=38;5;203:*.zip=38;5;203:*.z=38;5;203:*.dz=38;5;203:*.gz=38;5;203:*.lrz=38;5;203:*.lz=38;5;203:*.lzo=38;5;203:*.xz=38;5;203:*.zst=38;5;203:*.tzst=38;5;203:*.bz2=38;5;203:*.bz=38;5;203:*.tbz=38;5;203:*.tbz2=38;5;203:*.tz=38;5;203:*.deb=38;5;203:*.rpm=38;5;203:*.jar=38;5;203:*.war=38;5;203:*.ear=38;5;203:*.sar=38;5;203:*.rar=38;5;203:*.alz=38;5;203:*.ace=38;5;203:*.zoo=38;5;203:*.cpio=38;5;203:*.7z=38;5;203:*.rz=38;5;203:*.cab=38;5;203:*.wim=38;5;203:*.swm=38;5;203:*.dwm=38;5;203:*.esd=38;5;203:*.doc=38;5;109:*.docx=38;5;109:*.pdf=38;5;109:*.txt=38;5;109:*.md=38;5;109:*.rtf=38;5;109:*.odt=38;5;109:*.yaml=38;5;172:*.yml=38;5;172:*.json=38;5;172:*.toml=38;5;172:*.conf=38;5;172:*.config=38;5;172:*.ini=38;5;172:*.env=38;5;172:*.jpg=38;5;132:*.jpeg=38;5;132:*.png=38;5;132:*.gif=38;5;132:*.bmp=38;5;132:*.tiff=38;5;132:*.svg=38;5;132:*.mp3=38;5;72:*.wav=38;5;72:*.aac=38;5;72:*.flac=38;5;72:*.ogg=38;5;72:*.m4a=38;5;72:*.mp4=38;5;72:*.avi=38;5;72:*.mov=38;5;72:*.mkv=38;5;72:*.flv=38;5;72:*.wmv=38;5;72:*.c=38;5;142:*.cpp=38;5;142:*.py=38;5;142:*.java=38;5;142:*.js=38;5;142:*.ts=38;5;142:*.go=38;5;142:*.rs=38;5;142:*.php=38;5;142:*.html=38;5;142:*.css=38;5;142::*.nix=38;5;142:*.rs=38;5;142di=38;5;109:ur=38;5;223:uw=38;5;203:ux=38;5;142:ue=38;5;142:gr=38;5;223:gw=38;5;203:gx=38;5;142:tr=38;5;223:tw=38;5;203:tx=38;5;142:su=38;5;208:sf=38;5;208:xa=38;5;108:nb=38;5;244:nk=38;5;108:nm=38;5;172:ng=38;5;208:nt=38;5;203:ub=38;5;244:uk=38;5;108:um=38;5;172:ug=38;5;208:ut=38;5;203:lc=38;5;208:lm=38;5;208:uu=38;5;223:gu=38;5;223:un=38;5;223:gn=38;5;223:da=38;5;109:ga=38;5;108:gm=38;5;109:gd=38;5;203:gv=38;5;142:gt=38;5;108:gi=38;5;244:gc=38;5;203:Gm=38;5;108:Go=38;5;172:Gc=38;5;142:Gd=38;5;203:xx=38;5;237'

    # Kitty shell integration
    if set -q KITTY_INSTALLATION_DIR
        set --global KITTY_SHELL_INTEGRATION enabled no-sudo
        source "$KITTY_INSTALLATION_DIR/shell-integration/fish/vendor_conf.d/kitty-shell-integration.fish"
        set --prepend fish_complete_path "$KITTY_INSTALLATION_DIR/shell-integration/fish/vendor_completions.d"
    end

    set -gx EDITOR ${editor}/bin/nvim
    set -gx SHELL ${fishShell}/bin/fish
    set -gx TERM xterm-256color

    set -gx BAT_THEME gruvbox-dark

    set -gx MANPAGER "sh -c 'col -bx | bat --language man --style plain'"

    set -xg FZF_CTRL_T_COMMAND "fd --type file --hidden 2>/dev/null | sed 's#^\./##'"
    set -xg FZF_DEFAULT_OPTS '--prompt="🔭 " --height 80% --layout=reverse --border'
    set -xg FZF_DEFAULT_COMMAND 'rg --files --no-ignore --hidden --follow --glob "!.git/"'

    set -gx NIXOS_OZONE_WL 1

    # Ensure generated_completions is always in $fish_complete_path
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

    # /home/$user/.config/fish/config.fish
    if test (pwd) != "/workspace"
      cd /workspace
    end
end
''
