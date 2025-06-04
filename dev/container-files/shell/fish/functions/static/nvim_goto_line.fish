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
