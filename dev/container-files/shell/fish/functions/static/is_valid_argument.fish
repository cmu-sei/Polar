function is_valid_argument --description="Checks if it has been passed a valid argument"
    # Is there a valid argument?
    if test (count $argv) -gt 0
        echo "true"
    else
        echo "false"
    end
end
