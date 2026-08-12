require "minitest/autorun"
require "pathname"
require "tmpdir"
require_relative "../lib/upload"
require_relative "interrupt_test_helper"

class UploadNormalizationInterruptTest < Minitest::Test
  include InterruptTestHelper

  def test_call_restores_local_paths_when_interrupted_after_normalization
    Dir.mktmpdir do |directory_path|
      directory, file, normalized_file = upload_paths(directory_path)

      run_interrupted_upload(directory, file, normalized_file)

      assert_equal "content", file.read
      refute normalized_file.exist?
    end
  end

  private

  def upload_paths(directory_path)
    directory = Pathname.new(directory_path).join("bucket")
    directory.mkdir
    file = directory.join("source.txt")
    normalized_file = directory.join("normalized.txt")
    file.write("content")
    [directory, file, normalized_file]
  end

  def stub_normalization(file, normalized_file, trigger, &)
    plan = [[file.to_path, normalized_file.to_path]]
    Pathname.stub(:normalization_plan, plan) do
      Pathname.stub(:apply_normalization, interrupted_normalization(trigger), &)
    end
  end

  def run_interrupted_upload(directory, file, normalized_file)
    with_interrupt_after_side_effect do |trigger|
      uploader = Cloud::Upload.new("project")
      uploader.stub(:upload_files_by_directory, { directory => [file] }) do
        stub_cloud_upload(file, normalized_file, trigger) { assert_raises(Interrupt) { uploader.call } }
      end
    end
  end

  def stub_cloud_upload(file, normalized_file, trigger, &)
    Cloud.stub(:login, nil) do
      Cloud.stub(:exec, ->(_command) {}) do
        stub_normalization(file, normalized_file, trigger, &)
      end
    end
  end

  def interrupted_normalization(trigger)
    original_apply = Pathname.method(:apply_normalization)
    lambda do |plan|
      normalized = original_apply.call(plan)
      trigger.call
      normalized
    end
  end
end
