require "minitest/autorun"
require_relative "../lib/cloud"

class CloudTest < Minitest::Test
  def test_exec_raises_when_the_remote_command_fails
    Cloud.stub(:system, false) do
      error = assert_raises(Cloud::CommandError) { Cloud.exec("gcloud auth login") }

      assert_includes error.message, "Cloud.exec"
      assert_includes error.message, "gcloud auth login"
    end
  end

  def test_pipe_raises_when_the_remote_command_fails
    Cloud.stub(:ssh_command, "sh -c") do
      error =
        assert_raises(Cloud::CommandError) do
          Cloud.pipe("exit 7", &:read)
        end

      assert_equal "Cloud.pipe failed for \"exit 7\" (exit status: 7)", error.message
    end
  end

  def test_pipe_requires_a_block_to_check_the_remote_command_status
    assert_raises(ArgumentError) { Cloud.pipe("exit 7") }
  end

  def test_pipe_returns_the_block_result_when_the_remote_command_succeeds
    Cloud.stub(:ssh_command, "sh -c") do
      output = Cloud.pipe("printf success", &:read)

      assert_equal "success", output
    end
  end

  def test_pipe_does_not_print_the_remote_command
    stdout, =
      capture_io do
        Cloud.stub(:ssh_command, "sh -c") do
          Cloud.pipe("printf secret", &:read)
        end
      end

    assert_empty stdout
  end

  def test_ssh_command_uses_key_authentication_and_host_verification
    command = Cloud.__send__(:ssh_command)

    assert_includes command, "-i /run/googlecloud-ssh/client_key"
    assert_includes command, "-o ConnectionAttempts=5"
    assert_includes command, "-o ConnectTimeout=5"
    assert_includes command, "-o StrictHostKeyChecking=yes"
    assert_includes command, "-o UserKnownHostsFile=/run/googlecloud-ssh/known_hosts"
    assert_includes command, "cloud@googlecloud"
    refute_includes command, "sshpass"
    refute_includes command, "root@googlecloud"
  end
end
