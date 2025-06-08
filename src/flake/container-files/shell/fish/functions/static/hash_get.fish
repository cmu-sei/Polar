function hash_get --description="Return a hash of the input string."
    echo -n $argv[1] | sha1sum | cut -d ' ' -f 1
end
