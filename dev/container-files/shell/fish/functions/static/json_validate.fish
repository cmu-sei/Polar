function json_validate --description="Validate provided json against provided schema. E.g., 'validate_json file.json schema.json'. Can also handle json input from pipe via stdin."
    # jsonschema only operates via file input, which is inconvenient.
    if ! isatty stdin
        set -l tmp_file get_random_filename
        read > /tmp/$tmp_file; or exit -1
        jsonschema -F "{error.message}" -i /tmp/$tmp_file $argv[1]
        rm -f /tmp/$tmp_file
    else
        jsonschema -F "{error.message}" -i $argv[1] $argv[2]
    end
end
