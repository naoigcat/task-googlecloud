require "shellwords"
require_relative "cloud"

module Cloud
  class Upload
    # Keep construction and execution together so CLI entry points do not duplicate the workflow.
    def self.call(project)
      new(project).call
    end

    # Keep the project fixed on the instance so it cannot change during an upload.
    def initialize(project)
      @project = project
    end

    # Build the complete rename plan before authentication so a collision cannot cause partial changes.
    def call
      files_by_directory = upload_files_by_directory
      files = files_by_directory.values.flatten
      normalization_plan = Pathname.normalization_plan(files)

      authenticate_project
      normalized_files = files.zip(Pathname.apply_normalization(normalization_plan)).to_h
      upload_normalized_files(files_by_directory, normalized_files)
    end

    private

    # Collect every upload path up front so collision detection can see the complete set.
    def upload_files_by_directory
      Pathname.new("../uploads").expand_path(__dir__).glob("*")
              .to_h { |directory| [directory, directory.glob("*")] }
    end

    # Authenticate only after the rename plan is known to be collision-free.
    def authenticate_project
      Cloud.login
      Cloud.exec(Shellwords.join(["gcloud", "config", "set", "project", @project]))
    end

    # Upload using the post-normalization paths while preserving each directory-to-bucket mapping.
    def upload_normalized_files(files_by_directory, normalized_files)
      files_by_directory.each do |directory, directory_files|
        puts directory
        bucket = directory.basename.to_path
        directory_files.each do |file|
          Cloud.exec(Shellwords.join(["gsutil", "cp", normalized_files[file].to_path, "gs://#{bucket}"]))
        end
      end
    end
  end
end
