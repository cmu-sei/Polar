#!/usr/bin/env bash
set -euo pipefail

##############################################################################
# 0. tiny helpers
##############################################################################
die() { echo >&2 "error: $*"; exit 1; }
need() { command -v "$1" >/dev/null || die "missing binary: $1"; }

##############################################################################
# 1. create the normal dev user
##############################################################################
create_user() {
    local user=$1 uid=$2 gid=$3

    [[ -z $user || -z $uid || -z $gid ]] && die "Usage: $0 <user> <uid> <gid>"

    local myshell=/bin/fish

    sed -i "s|^$user:.*|$user:x:$uid:$gid::/home/$user:$myshell|" /etc/passwd
    echo "$user:!x:::::::"                   >> /etc/shadow

    # skeleton HOME
    mkdir -p  /home/$user
    chmod -R 755 /home/$user
    chown -R "$uid:$gid" /home/$user

    # XDG dirs before fish starts so it can write history, etc.
    install -d -m 700  \
        /home/$user/.config \
        /home/$user/.local/share \
        /home/$user/.cache \
        /home/$user/.ssh

    chown "$uid:$gid" \
        /home/$user/.config \
        /home/$user/.local/share \
        /home/$user/.cache \
        /home/$user/.ssh

    # bring over useful root files but NOT its fish config
    need rsync
    rsync -a --chown=$uid:$gid --exclude '.config/fish*' \
        /root/ /home/$user/ || true

    chmod 1777 /tmp                          # sticky-bit temp

    install -Dm644 /etc/container-skel/config.fish \
                   /home/$user/.config/fish/config.fish
    touch /home/$user/.config/fish/fish_variables
    chown -R "$uid:$gid" /home/$user
    chmod -R 755 /home/$user
    chmod u+w /home/$user
}

##############################################################################
# 2. main startup logic
##############################################################################
(( EUID == 0 )) || die "please run as root"

if [[ -n "${CREATE_USER:-}" && -n "${CREATE_UID:-}" && -n "${CREATE_GID:-}" ]]; then
    create_user "$CREATE_USER" "$CREATE_UID" "$CREATE_GID"
    DEV_USER=$CREATE_USER
    DEV_UID=$CREATE_UID
    DEV_GID=$CREATE_GID
else
    DEV_USER=root
    DEV_UID=0
    DEV_GID=0
fi

# Ensure a container-owned cargo target dir (avoid writing into host bind-mount).
CARGO_TARGET_DIR="/var/cache/cargo-target"
mkdir -p "$CARGO_TARGET_DIR"
chown "$DEV_UID:$DEV_GID" "$CARGO_TARGET_DIR"
chmod 0755 "$CARGO_TARGET_DIR"
export CARGO_TARGET_DIR

##############################################################################
# 3. build users
##############################################################################
# Append the dev user as a trusted nix user. Uses $DEV_USER directly rather
# than $USER to avoid depending on the environment variable being set correctly.
printf "\nextra-trusted-users = %s\n" "$DEV_USER" >> /etc/nix/nix.conf

# Configure Nix for the container's architecture at runtime. This is done
# here rather than at image build time so that the same start.sh ships in
# both the x86 and arm64 images and self-configures correctly on first run.
#
# On aarch64: tell Nix its system, and advertise x86_64-linux as an
# extra platform so arm64 developers can build and push x86 container
# images from within the arm64 dev container (e.g. on Apple Silicon).
# Docker Desktop on Apple Silicon provides x86_64 emulation via Rosetta 2,
# which satisfies these builds transparently.
#
# On x86: no extra configuration needed — x86 is the native platform and
# arm64 builds are handled by the host's binfmt/qemu registration.
CONTAINER_ARCH="$(uname -m)"
if [[ "$CONTAINER_ARCH" == "aarch64" ]]; then
    printf "\nsystem = aarch64-linux\n"          >> /etc/nix/nix.conf
    printf "\nextra-platforms = x86_64-linux\n"  >> /etc/nix/nix.conf

    # Detect qemu-user emulation via the CPU implementer field in /proc/cpuinfo.
    # Real aarch64 hardware never reports implementer 0x00 — this is qemu's
    # synthetic ARM CPU signature. Known real implementer codes:
    #   0x41 = ARM Ltd (Graviton, most server ARM)
    #   0x51 = Qualcomm (Snapdragon)
    #   0x61 = Apple (Apple Silicon)
    #   0x00 = qemu-user synthetic CPU — only possible value here
    #
    # When running under qemu-user, seccomp BPF programs fail to load because
    # they are arch-specific and the host kernel is x86. Disabling the sandbox
    # is safe in this context because the container itself provides isolation.
    # On native aarch64 (Apple Silicon, Graviton, etc.) this block is skipped
    # and the Nix sandbox operates normally.
    CPU_IMPLEMENTER=$(grep "CPU implementer" /proc/cpuinfo | head -1 | awk '{print $NF}')
    if [[ "$CPU_IMPLEMENTER" == "0x00" ]]; then
        printf "\nsandbox = false\n"        >> /etc/nix/nix.conf

        # The OCI container runtime (podman/runc) already applies a seccomp profile
        # to the container. Nix's own syscall filtering is redundant in any container
        # context, and actively broken under qemu-user where Nix generates an aarch64
        # BPF program that the x86 host kernel refuses to load. Disable it for
        # all aarch64 container environments — the runtime's filter is
        # sufficient. For x86_64 systems, it's redundant but harmless and also
        # the default behavior, so we leave it alone.
        printf "\nfilter-syscalls = false\n" >> /etc/nix/nix.conf
    fi
fi

# Create a group for the nix build users
echo "nixbld:x:30000:" >> /etc/group
echo "nixbld:x::"      >> /etc/gshadow

# Detect CPU count
cpus=$(command -v nproc >/dev/null 2>&1 && nproc || getconf _NPROCESSORS_ONLN)

# Ensure the dummy home & shell exist
mkdir -p /var/empty
DUMMY_SHELL=/bin/nologin
[ -x "$DUMMY_SHELL" ] || DUMMY_SHELL=/bin/false

members=()

for i in $(seq 1 "$cpus"); do
  muid=$((30000 + i))
  mname="nixbld$i"

  if ! getent passwd "$mname" >/dev/null; then
    printf '%s:x:%d:30000:Nix build user %d:/var/empty:%s\n' \
           "$mname" "$muid" "$i" "$DUMMY_SHELL" >> /etc/passwd
  fi

  members+=("$mname")
done

member_list=$(IFS=, ; echo "${members[*]}")

grep -v '^nixbld:' /etc/group   > /etc/group.new
grep -v '^nixbld:' /etc/gshadow > /etc/gshadow.new 2>/dev/null || true

echo "nixbld:x:30000:${member_list}" >> /etc/group.new
echo "nixbld:!:${member_list}:"      >> /etc/gshadow.new 2>/dev/null || true

mv -f /etc/group.new   /etc/group
mv -f /etc/gshadow.new /etc/gshadow 2>/dev/null || true

# The Nix DB is pre-populated inside the store by the nixDbRegistration
# derivation at image build time. However, the store is read-only, and the
# Nix daemon requires a writable DB to acquire big-lock and register new
# paths during builds. Any operation that triggers a real build (e.g.
# 'direnv allow', 'nix build', 'nix shell') will fail or corrupt the DB
# state if it tries to write through the store symlinks.
#
# Solution: copy the pre-populated DB out of the store into real writable
# files on the container's upper overlay layer before the daemon starts.
# This is the correct runtime/buildtime split — the store provides the
# seed data, the upper layer provides the mutable runtime state, exactly
# as NixOS itself does (the DB on a real NixOS system lives at
# /nix/var/nix/db/ as real files, not store symlinks).
#
# The symlink check ensures this only runs on first startup. If the DB has
# already been materialized (e.g. on a subsequent exec into a running
# container) we leave it alone.
if [[ -L /nix/var/nix/db/db.sqlite ]]; then
    db_src="$(dirname "$(readlink -f /nix/var/nix/db/db.sqlite)")"
    tmp="$(mktemp -d)"
    cp -r "$db_src/." "$tmp/"
    rm -f   /nix/var/nix/db/db.sqlite \
            /nix/var/nix/db/db.sqlite-shm \
            /nix/var/nix/db/db.sqlite-wal \
            /nix/var/nix/db/big-lock \
            /nix/var/nix/db/reserved \
            /nix/var/nix/db/schema
    cp -r "$tmp/." /nix/var/nix/db/
    rm -rf "$tmp"
    chmod 644 /nix/var/nix/db/db.sqlite
    chmod 600 /nix/var/nix/db/big-lock
    chmod 600 /nix/var/nix/db/reserved
fi

# Start the nix-daemon if not running
if ! pgrep -x nix-daemon >/dev/null ; then
    PATH=/nix/var/nix/profiles/default/bin:$PATH \
        /bin/nix-daemon --daemon &
fi

##############################################################################
# 3.5. Optional dropbear auto-start
##############################################################################
DROPBEAR_STATUS='autorun not configured — run "ssh-start" to start manually'

if [[ "${DROPBEAR_ENABLE:-0}" == "1" ]]; then
  SSH_DIR="/home/$DEV_USER/.ssh"
  AUTH_KEYS="$SSH_DIR/authorized_keys"
  RSA_KEY="$SSH_DIR/dropbear_rsa_host_key"
  ED25519_KEY="$SSH_DIR/dropbear_ed25519_host_key"
  DROPBEAR_PORT="${DROPBEAR_PORT:-2223}"

  mkdir -p "$SSH_DIR"
  chmod 700 "$SSH_DIR"
  chown "$DEV_UID:$DEV_GID" "$SSH_DIR"

  if [[ -n "${AUTHORIZED_KEYS_B64:-}" ]]; then
    echo "$AUTHORIZED_KEYS_B64" | base64 -d > "$AUTH_KEYS"
    chmod 600 "$AUTH_KEYS"
    chown "$DEV_UID:$DEV_GID" "$AUTH_KEYS"
  else
    DROPBEAR_STATUS="❌ authorized_keys missing (env var AUTHORIZED_KEYS_B64 not set)"
  fi

  if [[ ! -f "$RSA_KEY" ]]; then
    dropbearkey -t rsa -f "$RSA_KEY" > /dev/null
  fi

  if [[ ! -f "$ED25519_KEY" ]]; then
    dropbearkey -t ed25519 -f "$ED25519_KEY" > /dev/null
  fi

  if [[ -f "$AUTH_KEYS" ]]; then
    dropbear -E -a \
      -r "$RSA_KEY" \
      -r "$ED25519_KEY" \
      -p "0.0.0.0:$DROPBEAR_PORT" \
      -P "$SSH_DIR/dropbear.pid" &

    sleep 1
    if pgrep -x dropbear >/dev/null 2>&1; then
      DROPBEAR_STATUS="running on port $DROPBEAR_PORT"
    else
      DROPBEAR_STATUS="❌ failed to start (check logs)"
    fi
  fi
fi

##############################################################################
# 4. summary banner
##############################################################################
echo "───────────────────────────────────────────────────────────────────────────────"
echo " 🚀  Container ready!"
echo
echo " • User ............. $DEV_USER  (uid=$DEV_UID / gid=$DEV_GID)"
echo " • SSH server ....... $DROPBEAR_STATUS"

if mountpoint -q /workspace 2>/dev/null; then
  echo " • Volume mount ..... /workspace is mounted"
else
  echo " • Volume mount ..... none detected"
fi

echo
echo " ✅  Environment configuration complete"
echo " 🆘  Type 'polar-help' for container usage instructions"
echo "───────────────────────────────────────────────────────────────────────────────"

##############################################################################
# 5. hand control to the user shell
##############################################################################
HOME=/home/$DEV_USER \
  LOGNAME=$DEV_USER \
  SHELL=/bin/fish \
  USER=$DEV_USER \
  XDG_CACHE_HOME=/home/$DEV_USER/.cache \
  XDG_CONFIG_HOME=/home/$DEV_USER/.config \
  XDG_DATA_HOME=/home/$DEV_USER/.local/share \
  chroot --userspec="$DEV_UID:$DEV_GID" / /bin/fish -l
