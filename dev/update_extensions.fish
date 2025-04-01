#!/usr/bin/env fish

# Global Error handler
function handle_error
    set message $argv[1]
    echo "Error: $message" >&2
    cleanup
    exit 1
end

# Cleanup function
function cleanup
    echo "Cleaning up..."
    for file in $files
        rm -f $file
    end
    for filtered in $filtered_files
        rm -f $filtered
    end
end

# Source URLs
set sources \
    https://raw.githubusercontent.com/nix-community/nix-vscode-extensions/master/data/cache/vscode-marketplace-latest.json \
    https://raw.githubusercontent.com/nix-community/nix-vscode-extensions/master/data/cache/open-vsx-latest.json

# Download files
for source in $sources
    echo "Downloading $source..."
    curl -O $source; or handle_error "Download failed for $source!"
end

# Extract filenames from URLs
set files
set filtered_files
set prefixes
for source in $sources
    set filename (basename $source)

    # ✅ Fix: Use `string replace --` to avoid option misinterpretation
    set base_prefix (string replace -- "-latest.json" "" $filename)

    if test -z "$base_prefix" -o "$base_prefix" = "$filename"
        handle_error "Failed to extract base prefix from $filename"
    end

    # ✅ Generate correct filtered filename
    set filtered_filename "$base_prefix-nix.txt"

    # ✅ Assign correct Nix prefix based on the base name
    switch $base_prefix
        case "vscode-marketplace"
            set nix_prefix "extensions.vscode-marketplace"
        case "open-vsx"
            set nix_prefix "extensions.open-vsx-release"
        case '*'
            handle_error "Unknown extension source: $base_prefix"
    end

    # ✅ Store values correctly
    set files $files $filename
    set filtered_files $filtered_files $filtered_filename
    set prefixes $prefixes $nix_prefix
end

# Generate Nix-friendly lists directly from the downloaded JSON files
for index in (seq (count $files))
    set file $files[$index]
    set filtered_file $filtered_files[$index]
    set prefix $prefixes[$index]  # ✅ Use the correct precomputed prefix

    echo "Generating Nix extensions list for $file..."
    
    # ✅ Correct string formatting in `jq`
    jq -r ".[] | \"$prefix.\(.publisher).\(.name)\"" $file > $filtered_file; or handle_error "jq failed for $file!"
end

# Remove original unfiltered JSON files
for file in $files
    echo "Removing unfiltered original for $file..."
    rm -f $file; or handle_error "cleanup failed!"
end

echo "Script completed successfully!"
