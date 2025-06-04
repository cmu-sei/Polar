function var_erase --description="Fish shell is missing a proper ability to delete a var from all scopes."
    set -el $argv[1]
    set -eg $argv[1]
    set -eU $argv[1]
end
