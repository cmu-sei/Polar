function prettyjson --description="Pretty print JSON output"
    python -m json.tool $argv[1]
end
