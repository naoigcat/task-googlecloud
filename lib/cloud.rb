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
      return unless pipe("gcloud config get account", &:read).empty?

      exec "gcloud auth login"
    end

    def logout
      return if pipe("gcloud config get account", &:read).empty?

      exec "gcloud auth revoke && rm -fr /root/.config/gcloud"
    end

    def exec(command, _mode = "r", _opt = {}, &)
      success = system("#{sshpass} #{command.shellescape}")
      return if success

      raise CommandError.new("Cloud.exec", command, $CHILD_STATUS)
    end

    def pipe(command, mode = "r", opt = {}, &)
      raise ArgumentError, "Cloud.pipe requires a block to check command status" unless block_given?

      remote_command = "#{sshpass} #{command.shellescape}".tap(&method(:puts))
      result = IO.popen(remote_command, mode, opt, &)
      raise_command_error("Cloud.pipe", command, $CHILD_STATUS)
      result
    end

    private

    def raise_command_error(operation, command, status)
      return if status&.success?

      raise CommandError.new(operation, command, status)
    end

    def sshpass
      "sshpass -p secret ssh -o StrictHostKeyChecking=no root@googlecloud"
    end
  end
end
