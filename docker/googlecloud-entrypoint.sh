#!/bin/sh
set -eu

ssh_key_dir=/run/googlecloud-ssh

install -d -m 700 "$ssh_key_dir"
rm -f "$ssh_key_dir/client_key" "$ssh_key_dir/client_key.pub" "$ssh_key_dir/known_hosts"
ssh-keygen -A
ssh-keygen -q -t ed25519 -N "" -f "$ssh_key_dir/client_key"

known_hosts="$ssh_key_dir/known_hosts"
: > "$known_hosts"
for host_key in /etc/ssh/ssh_host_*_key.pub; do
  [ -f "$host_key" ] || continue
  printf 'googlecloud %s\n' "$(cat "$host_key")" >> "$known_hosts"
done
chmod 644 "$known_hosts"

install -d -m 700 -o cloud -g cloud /home/cloud/.ssh
install -m 600 -o cloud -g cloud "$ssh_key_dir/client_key.pub" /home/cloud/.ssh/authorized_keys

exec /usr/sbin/sshd -D -e
