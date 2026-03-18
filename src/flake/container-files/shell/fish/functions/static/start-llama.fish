function start-llama --description "Start llama.cpp server with a model (ROCm GPU accelerated)"
    # Usage:
    #   start-llama /workspace/models/mymodel.gguf
    #   start-llama --hf unsloth/Qwen3-Coder-Next-GGUF:D-IQ2_XXS
    #
    # Environment variables:
    #   LLAMA_PORT       port to listen on (default: 8080)
    #   LLAMA_CTX_SIZE   context size in tokens (default: 32768)
    #   LLAMA_GPU_LAYERS number of layers to offload to GPU (default: 99)
    #   LLAMA_HOST       bind address (default: 0.0.0.0)

    set -l port     (test -n "$LLAMA_PORT"       && echo "$LLAMA_PORT"       || echo "8080")
    set -l ctx      (test -n "$LLAMA_CTX_SIZE"   && echo "$LLAMA_CTX_SIZE"   || echo "32768")
    set -l gpu_lay  (test -n "$LLAMA_GPU_LAYERS" && echo "$LLAMA_GPU_LAYERS" || echo "99")
    set -l host     (test -n "$LLAMA_HOST"       && echo "$LLAMA_HOST"       || echo "0.0.0.0")

    # Parse arguments
    set -l hf_flag ""
    set -l model_path ""

    if test (count $argv) -eq 0
        echo "Usage: start-llama [--hf <repo:quant>] [<model.gguf>]"
        echo ""
        echo "Examples:"
        echo "  start-llama /workspace/models/mymodel.gguf"
        echo "  start-llama --hf unsloth/Qwen3-Coder-Next-GGUF:D-IQ2_XXS"
        return 1
    end

    if test "$argv[1]" = "--hf"
        if test (count $argv) -lt 2
            echo "Error: --hf requires a repo:quant argument"
            return 1
        end
        set hf_flag "--hf-repo" (string split ":" $argv[2])[1] "--hf-file" (string split ":" $argv[2])[2]
    else
        set model_path $argv[1]
        if not test -f "$model_path"
            echo "Error: model file not found: $model_path"
            echo "Tip: place models in /workspace/models/"
            return 1
        end
    end

    echo "Starting llama-server on $host:$port"
    echo "Context size: $ctx tokens | GPU layers: $gpu_lay"
    echo ""

    if test -n "$model_path"
        sudo llama-server \
            --model $model_path \
            --host $host \
            --port $port \
            --ctx-size $ctx \
            --n-gpu-layers $gpu_lay \
            --flash-attn \
            --alias "local-model"
    else
        sudo llama-server \
            $hf_flag \
            --host $host \
            --port $port \
            --ctx-size $ctx \
            --n-gpu-layers $gpu_lay \
            --flash-attn \
            --alias "local-model"
    end
end

