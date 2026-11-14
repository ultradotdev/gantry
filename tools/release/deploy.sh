#!/bin/sh
# Signs a release build and installs it on a Mac reached over SSH, where
# Gantry already runs as a service. Usage: deploy.sh SSH_HOST
#
# The binary is copied under a new name and renamed into place, never
# written over a running one. The service then restarts on the new build,
# after which it needs the Protect password again: run `gantry unlock`.
set -eu
host="${1:?usage: deploy.sh SSH_HOST}"
cd "$(dirname "$0")/../.."
sh tools/release/sign.sh
scp -q target/release/gantry "$host:gantry.new"
ssh "$host" 'mv -f ~/gantry.new ~/gantry && ~/gantry service install'
echo
echo "Now give it the Protect password:"
echo "  ssh -t $host /usr/local/bin/gantry unlock"
