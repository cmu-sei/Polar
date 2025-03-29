#!/bin/bash

# Function to print usage information
usage() {
    echo "Usage: $0 <dhall_root_directory> <umbrella_chart_path>"
    exit 1
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
        yaml_file="${dhall_file%.dhall}.yaml"

        echo "[INFO] Converting: $dhall_file -> $yaml_file"

        if ! dhall-to-yaml --file "$dhall_file" > "$yaml_file"; then
            echo "[ERROR] Error converting $dhall_file" >&2
            return 1
        fi
    done

    echo "[SUCCESS] All Dhall files in '$dhall_dir' converted successfully."
    return 0
}

# Ensure both arguments are provided
if [ $# -lt 2 ]; then
    echo "Error: Missing required arguments."
    usage
fi

# Assign arguments to variables
DHALL_ROOT="$1"
UMBRELLA_CHART_NAME="$2"

# Ensure the provided Dhall directory exists
if [[ ! -d "$DHALL_ROOT" ]]; then
    echo "Error: '$DHALL_ROOT' is not a valid directory." >&2
    exit 1
fi

# Print confirmation of inputs
echo "📂 Dhall root directory: $DHALL_ROOT"
echo "📛 Umbrella chart name: $UMBRELLA_CHART_NAME"

# Iterate over each subdirectory in the Dhall root
for SERVICE_DIR in "$DHALL_ROOT"/*/; do
    # Skip if it's not a directory
    [[ -d "$SERVICE_DIR" ]] || continue  

    SERVICE_NAME=$(basename "$SERVICE_DIR")
    CHART_DHALL="$SERVICE_DIR/chart.dhall"

    echo "🔍 Checking service: $SERVICE_NAME"

    # Ensure chart.dhall exists in the service directory
    if [[ ! -f "$CHART_DHALL" ]]; then
        echo "[ERROR] Error: Missing 'chart.dhall' in '$SERVICE_DIR'." >&2
        exit 1
    fi

    echo "[SUCCESS] Found 'chart.dhall' in $SERVICE_NAME"
done

# Start generating the umbrella Helm chart
echo "🚀 Generating umbrella chart: $UMBRELLA_CHART_NAME"


#set up directory
CHILD_CHART_DEST_DIR="$UMBRELLA_CHART_NAME/charts"
mkdir -p "$CHILD_CHART_DEST_DIR"

# Convert the global Dhall chart definition to YAML
GLOBAL_CHART_DHALL="$DHALL_ROOT/chart.dhall"
GLOBAL_VALUES_DHALL="$DHALL_ROOT/values.dhall"
CHART_YAML="$UMBRELLA_CHART_NAME/Chart.yaml"

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
VALUES_YAML="$UMBRELLA_CHART_NAME/values.yaml"
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

echo "[SUCCESS] Umbrella chart setup complete at: $UMBRELLA_CHART_NAME"

# Process child charts
echo "🔍 Discovering and generating child charts..."
for SERVICE_DIR in "$DHALL_ROOT"/*/; do
    [[ -d "$SERVICE_DIR" ]] || continue  # Skip non-directories

    CHILD_CHART_NAME=$(basename "$SERVICE_DIR")
    CHILD_CHART_DHALL="$SERVICE_DIR/chart.dhall"
    CHILD_DHALL_DIR="${DHALL_ROOT}/${SERVICE_NAME}"
    
    echo "[INFO] Processing service: $SERVICE_NAME"
    
    if [[ ! -f "$CHILD_CHART_DHALL" ]]; then
        echo "[ERROR] Missing 'chart.dhall' in '$SERVICE_DIR'." >&2
        exit 1
    fi
    
    CHILD_CHART_DIR="$CHILD_CHART_DEST_DIR/$CHILD_CHART_NAME"
    
    echo "Writing chart for $SERVICE_NAME to $CHILD_CHART_DIR"
    mkdir -p "$CHILD_CHART_DIR/templates"

    # Convert service's chart.dhall to Chart.yaml
    CHILD_CHART_YAML="$CHILD_CHART_DIR/Chart.yaml"
    echo "[INFO] Converting $SERVICE_NAME 'chart.dhall' to 'Chart.yaml'..."
    if ! dhall-to-yaml --file "$CHILD_CHART_DHALL" > "$CHILD_CHART_YAML"; then
        echo "[ERROR] Failed to convert 'chart.dhall' to 'Chart.yaml' for $SERVICE_NAME." >&2
        exit 1
    fi

    echo "[SUCCESS] Generated 'Chart.yaml' for $SERVICE_NAME."
    
    # Convert all Dhall files in the service directory (excluding chart.dhall)
    echo "[INFO] Converting Dhall files in $SERVICE_DIR to YAML..."
    find "$SERVICE_DIR" -maxdepth 1 -type f -name "*.dhall" ! -name "chart.dhall" | while read -r dhall_file; do
        yaml_file="$CHILD_CHART_DIR/templates/$(basename "${dhall_file%.dhall}.yaml")"
        
        echo "   [INFO] Converting: $dhall_file -> $yaml_file"
        if ! dhall-to-yaml --file "$dhall_file" > "$yaml_file"; then
            echo "[ERROR] Error: Failed to convert $dhall_file" >&2
            exit 1
        fi
    done

    echo "[SUCCESS] Finished processing $CHILD_CHART_NAME."
done

# Run Helm linting
echo "Running Helm lint check on the umbrella chart..."
if ! helm lint "$UMBRELLA_CHART_NAME"; then
    echo "[ERROR] Helm linting failed!" >&2
fi

echo "[SUCCESS] Helm linting passed."

# Render Helm template
echo "🛠️ Rendering Helm template..."
if ! helm template "$UMBRELLA_CHART_NAME"; then
    echo "[ERROR] Helm template rendering failed!" >&2
fi

echo "🎉 Helm chart setup complete!"
echo "You can now install the chart using:"
echo "helm install $UMBRELLA_CHART_NAME $UMBRELLA_CHART_NAME"
