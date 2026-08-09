require "minitest/autorun"
require "yaml"

class ComposeTest < Minitest::Test
  EXPECTED_GEM_VERSIONS = [
    ["ast", [2, 4, 3]],
    ["json", [2, 21, 2]],
    ["language_server-protocol", [3, 17, 0, 6]],
    ["lint_roller", [1, 1, 0]],
    ["parallel", [2, 1, 0]],
    ["parser", [3, 3, 12, 0]],
    ["prism", [1, 9, 0]],
    ["racc", [1, 8, 1]],
    ["rainbow", [3, 1, 1]],
    ["regexp_parser", [2, 12, 0]],
    ["rubocop-ast", [1, 50, 0]],
    ["ruby-progressbar", [1, 13, 0]],
    ["unicode-emoji", [4, 2, 0]],
    ["unicode-display_width", [3, 2, 0]],
    ["rubocop", [1, 89, 0]],
  ].freeze
  private_constant :EXPECTED_GEM_VERSIONS

  def setup
    @compose = File.read(File.expand_path("../compose.yaml", __dir__))
    @compose_config = YAML.safe_load(@compose)
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

  def test_build_dependencies_use_explicit_versions
    app_dockerfile = @compose_config.dig("services", "app", "build", "dockerfile_inline")
    googlecloud_dockerfile = @compose_config.dig("services", "googlecloud", "build", "dockerfile_inline")

    assert_explicit_gem_versions(app_dockerfile)
    refute_includes app_dockerfile, "gem install rubocop --no-document"
    assert_includes googlecloud_dockerfile, "google-cloud-cli:489.0.0"
    refute_includes googlecloud_dockerfile, "google-cloud-cli:latest"
  end

  private

  def assert_explicit_gem_versions(dockerfile)
    EXPECTED_GEM_VERSIONS.each do |name, version_parts|
      version = version_parts.join(".")
      assert_includes dockerfile, "gem install #{name} -v #{version} --no-document"
    end
  end
end
