function lh --description 'An approximation of "ls -alh", but uses eza, a replacement for ls, with some useful options as defaults.'
    eza --group --header --group-directories-first --long --icons --git --all --binary --dereference --links $argv
end
