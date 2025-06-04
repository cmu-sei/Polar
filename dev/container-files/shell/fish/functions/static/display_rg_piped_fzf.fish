function display_rg_piped_fzf --description="Pipe ripgrep output into fzf"
    rg . -n --glob "!.git/" | fzf
end
