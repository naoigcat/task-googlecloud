require "minitest/autorun"

class ComposeTest < Minitest::Test
  def setup
    @compose = File.read(File.expand_path("../compose.yaml", __dir__))
    @entrypoint = File.read(File.expand_path("../docker/googlecloud-entrypoint.sh", __dir__))
  end

  def test_googlecloud_allows_only_key_authentication_for_a_non_root_user
    assert_includes @compose, "useradd --create-home --shell /bin/bash cloud"
    assert_includes @compose, "PermitRootLogin no"
    assert_includes @compose, "PasswordAuthentication no"
    assert_includes @compose, "AllowUsers cloud"
    assert_includes @compose, "00-googlecloud-security.conf"
    assert_includes @compose, "sshd -T"
    assert_includes @compose, "nc -z 127.0.0.1 22"
    refute_includes @compose, "root:secret"
    refute_includes @compose, "sshpass"
  end

  def test_googlecloud_creates_ephemeral_client_key_and_known_hosts
    assert_includes @entrypoint, "ssh-keygen -q -t ed25519 -N \"\""
    assert_includes @entrypoint, "ssh_host_*_key.pub"
    assert_includes @compose, "googlecloud-ssh:/run/googlecloud-ssh:ro"
    assert_includes @compose, "type: tmpfs"
  end
end
