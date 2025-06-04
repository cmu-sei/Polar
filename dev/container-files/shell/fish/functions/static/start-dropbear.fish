function start-dropbear
    set ssh_dir ~/.ssh
    set auth_keys $ssh_dir/authorized_keys
    set rsa_key $ssh_dir/dropbear_rsa_host_key
    set ed25519_key $ssh_dir/dropbear_ed25519_host_key

    if not test -f /workspace/authorized_keys
        echo "❌ /workspace/authorized_keys not found. Please copy your public key into the container."
        return 1
    end

    echo "🔧 Fixing permissions..."
    mkdir -p $ssh_dir
    chmod 700 $ssh_dir
    cp /workspace/authorized_keys $auth_keys
    chmod 600 $auth_keys

    if not test -f $rsa_key
        echo "🔑 Generating RSA host key..."
        dropbearkey -t rsa -f $rsa_key > /dev/null
    end

    if not test -f $ed25519_key
        echo "🔑 Generating ED25519 host key..."
        dropbearkey -t ed25519 -f $ed25519_key > /dev/null
    end

    echo "🚀 Starting Dropbear on 0.0.0.0:2222"
    dropbear -F -E -e -a -s -D ~/.ssh -r $rsa_key -r $ed25519_key -p 0.0.0.0:2223
end
