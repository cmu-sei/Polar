#!/bin/bash

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

        echo "[INFO] Converting: $dhall_file -> $yaml_file"

        if ! dhall-to-yaml --file "$dhall_file" > "$yaml_file"; then
            echo "[ERROR] Error converting $dhall_file" >&2
            return 1
        fi
    done

    echo "[SUCCESS] All Dhall files in '$dhall_dir' converted successfully."
    return 0
}
# Function to print usage information
usage() {
    echo "Usage: $0 <dhall_root_directory> <umbrella_chart_path> [--render-template]"
    exit 1
}

# Ensure at least two arguments are provided
if [ $# -lt 2 ]; then
    echo "Error: Missing required arguments."
    usage
fi

# Assign arguments
DHALL_ROOT="$1"
UMBRELLA_CHART_NAME="$2"
RENDER_TEMPLATE=false

if [ "$3" == "--render-templates" ]; then
    RENDER_TEMPLATE=true
fi

# Ensure necessary tools are installed
for cmd in dhall-to-yaml helm sops; do
    if ! command -v "$cmd" &> /dev/null; then
        echo "Error: $cmd is not installed. Install it and try again." >&2
        exit 1
    fi
done

# Ensure the provided Dhall directory exists
if [[ ! -d "$DHALL_ROOT" ]]; then
    echo "Error: '$DHALL_ROOT' is not a valid directory." >&2
    exit 1
fi

# Print confirmation of inputs
echo "📂 Dhall root directory: $DHALL_ROOT"
echo "📛 Umbrella chart name: $UMBRELLA_CHART_NAME"

# Start generating the umbrella Helm chart
echo "🚀 Generating umbrella chart: $UMBRELLA_CHART_NAME"

# Set up umbrella chart directory structure
UMBRELLA_CHART_DIR="$UMBRELLA_CHART_NAME"
CHILD_CHART_DEST_DIR="$UMBRELLA_CHART_DIR/charts"
TEMPLATES_DIR="$UMBRELLA_CHART_DIR/templates"
mkdir -p "$CHILD_CHART_DEST_DIR"
mkdir -p "$TEMPLATES_DIR"

# Convert the global Dhall chart definition to YAML
GLOBAL_CHART_DHALL="$DHALL_ROOT/chart.dhall"
GLOBAL_VALUES_DHALL="$DHALL_ROOT/values.dhall"
CHART_YAML="$UMBRELLA_CHART_DIR/Chart.yaml"

if [[ ! -f "$GLOBAL_CHART_DHALL" ]]; then
    echo "[ERROR] Error: Missing global 'chart.dhall' in '$DHALL_ROOT'." >&2
    exit 1
fi

echo "[INFO] Converting global 'chart.dhall' to 'Chart.yaml'..."
if ! dhall-to-yaml --file "$GLOBAL_CHART_DHALL" > "$CHART_YAML"; then
    echo "[ERROR] Error: Failed to convert 'chart.dhall' to 'Chart.yaml'." >&2
    exit 1
fi

echo "[SUCCESS] Generated 'Chart.yaml' for umbrella chart."

# Convert global values.dhall to values.yaml
VALUES_YAML="$UMBRELLA_CHART_DIR/values.yaml"
if [[ ! -f "$GLOBAL_VALUES_DHALL" ]]; then
    echo "[ERROR] Error: Missing global 'values.dhall' in '$DHALL_ROOT'." >&2
    exit 1
fi

echo "[INFO] Converting 'values.dhall' to 'values.yaml'..."
if ! dhall-to-yaml --file "$GLOBAL_VALUES_DHALL" > "$VALUES_YAML"; then
    echo "[ERROR] Error: Failed to convert 'values.dhall' to 'values.yaml'." >&2
    exit 1
fi

echo "[SUCCESS] Generated 'values.yaml' for umbrella chart."

echo "[INFO] Generating umbrella chart templates."
convert_dhall_to_yaml "$DHALL_ROOT/templates" "$UMBRELLA_CHART_DIR/templates"

echo "[SUCCESS] Umbrella chart setup complete at: $UMBRELLA_CHART_NAME"

# Process child charts
echo "🔍 Discovering and generating child charts..."
for SERVICE_DIR in "$DHALL_ROOT"/*/; do
    [[ -d "$SERVICE_DIR" ]] || continue  # Skip non-directories

    CHILD_CHART_NAME=$(basename "$SERVICE_DIR")
    CHILD_CHART_DHALL="$SERVICE_DIR/chart.dhall"
    
    echo "[INFO] Processing service: $CHILD_CHART_NAME"
    
    if [[ ! -f "$CHILD_CHART_DHALL" ]]; then
        echo "[WARNING] Missing 'chart.dhall' in '$SERVICE_DIR', skipping..." >&2
        continue
    fi
    
    CHILD_CHART_DIR="$CHILD_CHART_DEST_DIR/$CHILD_CHART_NAME"
    CHILD_TEMPLATES_DIR="$CHILD_CHART_DIR/templates"
    
    echo "Writing chart for $CHILD_CHART_NAME to $CHILD_CHART_DIR"
    mkdir -p "$CHILD_CHART_DIR"
    mkdir -p "$CHILD_TEMPLATES_DIR"

    # Convert service's chart.dhall to Chart.yaml
    CHILD_CHART_YAML="$CHILD_CHART_DIR/Chart.yaml"
    echo "[INFO] Converting $CHILD_CHART_NAME 'chart.dhall' to 'Chart.yaml'..."
    if ! dhall-to-yaml --file "$CHILD_CHART_DHALL" > "$CHILD_CHART_YAML"; then
        echo "[ERROR] Failed to convert 'chart.dhall' to 'Chart.yaml' for $CHILD_CHART_NAME." >&2
        exit 1
    fi

    echo "[SUCCESS] Generated 'Chart.yaml' for $CHILD_CHART_NAME."
    
    # Convert all Dhall files in the service directory (excluding chart.dhall)
    echo "[INFO] Converting Dhall files in $SERVICE_DIR to YAML..."
    find "$SERVICE_DIR" -maxdepth 1 -type f -name "*.dhall" ! -name "chart.dhall" | while read -r dhall_file; do
        yaml_file="$CHILD_TEMPLATES_DIR/$(basename "${dhall_file%.dhall}.yaml")"
        
        echo "   [INFO] Converting: $dhall_file -> $yaml_file"        
        if ! dhall-to-yaml --file "$dhall_file" > "$yaml_file"; then
            echo "[ERROR] Error: Failed to convert $dhall_file" >&2
            exit 1
        fi
        # TODO: Enable SOPS encryption w/ Azure key vault
        # If the file looks like a secret, encrypt it with SOPS
        # if [[ "$dhall_file" =~ -secret\.dhall$ ]]; then
        #     echo "   [INFO] Encrypting secret YAML with SOPS: $yaml_file"            
        #     if ! sops --encrypt --in-place "$yaml_file"; then
        #         echo "[ERROR] Failed to encrypt $yaml_file with SOPS" >&2
        #         exit 1
        #     fi
        # fi

    done

    echo "[SUCCESS] Finished processing $CHILD_CHART_NAME."
done

# Run Helm linting
echo "Running Helm lint check on the umbrella chart..."
if ! helm lint "$UMBRELLA_CHART_NAME"; then
    echo "[ERROR] Helm linting failed!" >&2
fi

echo "[SUCCESS] Helm linting passed."

if [ "$RENDER_TEMPLATE" = true ]; then
    echo "🛠️ Rendering Helm template..."
    if ! helm template "$UMBRELLA_CHART_NAME"; then
        echo "[ERROR] Helm template rendering failed!" >&2
        exit 1
    fi
    echo "[SUCCESS] Helm template rendered."
else
    echo "[INFO] Skipping Helm template rendering."
fi


echo "Helm chart setup complete!"
