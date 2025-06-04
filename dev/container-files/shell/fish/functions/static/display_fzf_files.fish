function display_fzf_files --description="Call fzf and preview file contents using bat."
    set preview_command "bat --theme=gruvbox-dark --color=always --style=header,grid --line-range :400 {}"
    fzf --ansi --preview $preview_command
end
