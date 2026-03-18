function pi-local --description "Run pi agent against local llama.cpp server"
    # Runs the pi coding agent using a local llama-server instance as the
    # LLM backend. The llama.cpp OpenAI-compatible API is used so pi treats
    # the local server as an OpenAI-compatible provider.
    #
    # Start the server first with: start-llama <model>
    #
    # Environment variables:
    #   LLAMA_BASE_URL   base URL of the llama server (default: http://localhost:8080/v1)
    #   LLAMA_MODEL      model name to report to pi (default: local-model)

    set -l base_url (test -n "$LLAMA_BASE_URL" && echo "$LLAMA_BASE_URL" || echo "http://localhost:8080/v1")
    set -l model    (test -n "$LLAMA_MODEL"    && echo "$LLAMA_MODEL"    || echo "local-model")

    # Verify the server is reachable before starting pi
    if not curl -sf "$base_url/models" >/dev/null 2>&1
        echo "Error: llama-server not reachable at $base_url"
        echo "Start it first with: start-llama <model>"
        return 1
    end

    env \
        OPENAI_API_KEY="local" \
        OPENAI_BASE_URL="$base_url" \
        pi --provider openai --model "$model" $argv
end
