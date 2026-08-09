require "shellwords"
require_relative "cloud"
require_relative "normalization_plan"

module Cloud
  class Normalize
    # Keep construction and execution together so CLI entry points do not duplicate the workflow.
    def self.call(project, bucket)
      new(project, bucket).call
    end

    # Keep the target fixed throughout the operation to avoid acting on a different bucket.
    def initialize(project, bucket)
      @project = project
      @bucket = bucket
    end

    # Validate every object before the first move to avoid partial changes when names collide.
    def call
      Cloud.login
      Cloud.exec(Shellwords.join(["gcloud", "config", "set", "project", @project]))
      files = Cloud.pipe(Shellwords.join(["gsutil", "ls", "gs://#{@bucket}/**"]), &:readlines).map(&:chomp)

      NormalizationPlan.build(files).each do |source, target|
        next if source == target

        Cloud.exec(Shellwords.join(["gsutil", "mv", source, target]))
        sleep 1
      end
    end
  end
end
