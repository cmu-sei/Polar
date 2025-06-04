function yaml_to_json --description="Converts YAML input to JSON output."
    python -c 'import sys, yaml, json; y=yaml.safe_load(sys.stdin.read()); print(json.dumps(y))' $argv[1] | read; or exit -1
end
