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
