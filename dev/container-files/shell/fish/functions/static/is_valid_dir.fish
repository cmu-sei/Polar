function is_valid_dir --description="Checks if the argument passed is a valid directory path"
    if test (is_valid_argument $argv) = "true" -a (path_exists $argv) = "true" -a (is_a_directory $argv) = "true"
        echo "true"
    else
        echo "false"
    end
end
