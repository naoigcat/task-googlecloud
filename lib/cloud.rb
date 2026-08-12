require "English"
require "shellwords"
require_relative "pathname_normalization"
require_relative "string_normalization"

module Cloud
  class CommandError < StandardError
    def initialize(operation, command, status)
      exit_status = status&.exitstatus || "unknown"
      super("#{operation} failed for #{command.inspect} (exit status: #{exit_status})")
    end
  end

  class << self
    def login
      exec "gcloud auth login" unless authenticated?
    end

    def logout
      return unless authenticated?

      exec "gcloud auth revoke && rm -fr /home/cloud/.config/gcloud"
    end

    def exec(command, _mode = "r", _opt = {}, &)
      success = system("#{ssh_command} #{command.shellescape}")
      return if success

      raise CommandError.new("Cloud.exec", command, $CHILD_STATUS)
    end

    def pipe(command, mode = "r", opt = {}, &)
      raise ArgumentError, "Cloud.pipe requires a block to check command status" unless block_given?

      remote_command = "#{ssh_command} #{command.shellescape}"
      result =
        begin
          IO.popen(remote_command, mode, opt, &)
        rescue IOError, SystemCallError
          raise CommandError.new("Cloud.pipe", command, $CHILD_STATUS)
        end
      raise_command_error("Cloud.pipe", command, $CHILD_STATUS)
      result
    end

    private

    def authenticated?
      account = pipe("gcloud config get account", &:read).strip
      !account.empty? && account != "(unset)"
    end

    def raise_command_error(operation, command, status)
      return if status&.success?

      raise CommandError.new(operation, command, status)
    end

    def ssh_command
      "ssh -i /run/googlecloud-ssh/client_key " \
        "-o IdentitiesOnly=yes " \
        "-o BatchMode=yes " \
        "-o ConnectionAttempts=5 " \
        "-o ConnectTimeout=5 " \
        "-o StrictHostKeyChecking=yes " \
        "-o UserKnownHostsFile=/run/googlecloud-ssh/known_hosts " \
        "cloud@googlecloud"
    end
  end
end
