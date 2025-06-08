#!/bin/bash
set -euo pipefail

convert_and_encrypt() {
    input_dhall="$1"
    output_yaml="$2"

    # Convert Dhall to YAML
    if ! dhall-to-yaml --file "$input_dhall" > "$output_yaml"; then
        echo "[ERROR] Failed to convert $input_dhall to YAML" >&2
        rm -f "$output_yaml"
        exit 1
    fi

    # Encrypt the YAML using SOPS with local .sops.yaml config
    if ! sops -e --config .sops.yaml --verbose --in-place --output-type yaml "$output_yaml"; then
        echo "[ERROR] Failed to encrypt $output_yaml with SOPS" >&2
        rm -f "$output_yaml"
        exit 1
    fi
}

convert_dhall_to_yaml() {
    local dhall_dir="$1"
    local output_dir="$2"

    # Ensure the directory exists
    if [[ ! -d "$dhall_dir" ]]; then
        echo "Error: Directory '$dhall_dir' does not exist." >&2
        return 1
    fi

    # Find and convert all .dhall files in the directory
    find "$dhall_dir" -maxdepth 1 -type f -name "*.dhall" | while read -r dhall_file; do
        # Extract the relative path of the file inside the dhall_dir
        relative_path="${dhall_file#$dhall_dir/}"

        # Generate the corresponding yaml file path
        yaml_file="$output_dir/$relative_path"
        yaml_file="${yaml_file%.dhall}.yaml"  # Change extension to .yaml

        # Create the necessary directories for the yaml file
        mkdir -p "$(dirname "$yaml_file")"

        # If the file looks like a secret, encrypt it with SOPS before we write it
        if [[ "$dhall_file" =~ -secret\.dhall$ ]]; then
            echo "[INFO] Encrypting secret from $dhall_file"
            convert_and_encrypt $dhall_file $yaml_file
        else
            echo "[INFO] Converting: $dhall_file -> $yaml_file"

            if ! dhall-to-yaml --file "$dhall_file" > "$yaml_file"; then
                echo "[ERROR] Error: Failed to convert $dhall_file" >&2
                exit 1
            fi
        fi

    done

    echo "[SUCCESS] All Dhall files in '$dhall_dir' converted successfully."
    return 0
}
# Function to print usage information
usage() {
    echo "Usage: $0 <dhall_root_directory> <output_directory>"
    exit 1
}

# Ensure at least two arguments are provided
if [ $# -lt 2 ]; then
    echo "Error: Missing required arguments."
    usage
fi

# Assign arguments
DHALL_ROOT="$1"
OUTPUT_DIR="$2"


# Ensure the provided Dhall directory exists
if [[ ! -d "$DHALL_ROOT" ]]; then
    echo "Error: '$DHALL_ROOT' is not a valid directory." >&2
    exit 1
fi

# Print confirmation of inputs
echo "📂 Dhall root directory: $DHALL_ROOT"
echo "Writing manifests into $OUTPUT_DIR"

mkdir -p "$OUTPUT_DIR"

convert_dhall_to_yaml "$DHALL_ROOT/global" "$OUTPUT_DIR"


# Process child charts
echo "🔍 Discovering and generating child charts..."
for SERVICE_DIR in "$DHALL_ROOT"/*/; do
    [[ -d "$SERVICE_DIR" ]] || continue  # Skip non-directories

    SERVICE_NAME=$(basename "$SERVICE_DIR")

    echo "[INFO] Processing service: $SERVICE_NAME"

    convert_dhall_to_yaml "$DHALL_ROOT/$SERVICE_NAME" "$OUTPUT_DIR/"

    echo "[SUCCESS] Finished processing $SERVICE_NAME."
done

echo "Rendering done!"
