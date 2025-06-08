function path_exists --description="Checks if the path exists"
    # Does it exist?
    if test -e $argv[1]
        echo "true"
    else
        echo "false"
    end
end
