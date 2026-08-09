require "securerandom"
require "shellwords"
require_relative "cloud"
require_relative "cloud_move"

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
      upload_with_rollback(files_by_directory, normalized_files, normalization_plan)
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

    def upload_with_rollback(files_by_directory, normalized_files, normalization_plan)
      upload_normalized_files(files_by_directory, normalized_files)
    rescue StandardError => e
      rollback_errors = Pathname.rollback_normalization(normalization_plan)
      NormalizationPlan.raise_on_rollback_failure(e, rollback_errors)
      raise
    end

    # Stage uploads before finalizing them so failed batches can remove remote changes.
    def upload_normalized_files(files_by_directory, normalized_files)
      staging_prefix = ".task-googlecloud-staging/#{SecureRandom.hex(16)}"
      staged_files = []
      finalized_files = []

      perform_upload(files_by_directory, normalized_files, staging_prefix, staged_files, finalized_files)
    rescue StandardError => e
      rollback_errors = rollback_uploads(staged_files, finalized_files)
      NormalizationPlan.raise_on_rollback_failure(e, rollback_errors)
      raise
    end

    def perform_upload(files_by_directory, normalized_files, staging_prefix, staged_files, finalized_files)
      files_by_directory.each do |directory, directory_files|
        stage_directory(directory, directory_files, normalized_files, staging_prefix, staged_files)
      end

      finalize_uploads(staged_files, finalized_files)
    end

    def stage_directory(directory, directory_files, normalized_files, staging_prefix, staged_files)
      puts directory
      bucket = directory.basename.to_path
      directory_files.each do |file|
        normalized_file = normalized_files[file]
        staging_path = "gs://#{bucket}/#{staging_prefix}/#{normalized_file.basename}"
        final_path = "gs://#{bucket}/#{normalized_file.basename}"
        staged_files << [staging_path, final_path]
        Cloud.exec(Shellwords.join(["gsutil", "cp", normalized_file.to_path, staging_path]))
      end
    end

    def finalize_uploads(staged_files, finalized_files)
      staged_files.each do |staging_path, final_path|
        finalized_files << [staging_path, final_path]
        Cloud.exec(Cloud::ObjectMove.command(staging_path, final_path))
      end
    end

    def rollback_uploads(staged_files, finalized_files)
      rollback_finalized_uploads(finalized_files) + cleanup_staged_uploads(staged_files)
    end

    def rollback_finalized_uploads(finalized_files)
      finalized_files.reverse_each.filter_map do |staging_path, final_path|
        attempt_remote_rollback(staging_path, final_path)
      end
    end

    def cleanup_staged_uploads(staged_files)
      staged_files.filter_map do |staged_file|
        attempt_remote_cleanup(staged_file.first)
      end
    end

    # Try every remote cleanup so one failure does not block later restorations.
    def attempt_remote_rollback(source, target)
      Cloud.exec(Cloud::ObjectMove.rollback_command(source, target))
    rescue StandardError => e
      e
    end

    def attempt_remote_cleanup(staging_path)
      Cloud.exec(Shellwords.join(["gsutil", "rm", "-f", staging_path]))
    rescue StandardError => e
      e
    end
  end
end
