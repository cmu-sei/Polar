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
