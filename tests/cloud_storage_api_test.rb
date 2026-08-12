require "minitest/autorun"
require "shellwords"
require_relative "../lib/cloud_move"

class CloudStorageApiTest < Minitest::Test
  SOURCE = "gs://bucket/folder*?[]#/source.txt".freeze
  TARGET = "gs://bucket/folder*?[]#/target.txt".freeze
  private_constant :SOURCE, :TARGET

  def test_move_passes_special_object_names_to_the_api_as_data
    command = Cloud::ObjectMove.command(SOURCE, TARGET, source_generation: "123")

    expected = Cloud::StorageApi.move_command(SOURCE, TARGET, source_generation: "123")
    assert_includes command, expected
    assert_includes command, Shellwords.join(["--source-object=folder*?[]#/source.txt"])
    refute_includes command, "gsutil"
  end

  def test_local_copy_passes_special_names_to_the_api_as_data
    command = Cloud::ObjectCopy.command("/tmp/source*?[]#.txt", TARGET)

    expected = Cloud::StorageApi.upload_command("/tmp/source*?[]#.txt", TARGET)
    assert_includes command, expected
    refute_includes command, "gsutil"
  end

  def test_rollback_uses_exact_api_copy_and_delete_operations
    command = Cloud::ObjectMove.rollback_command(SOURCE, TARGET, "456")

    expected = Cloud::StorageApi.copy_command(TARGET, SOURCE, source_generation: "456", destination_generation: "0")
    assert_includes command, expected
    assert_includes command, Cloud::StorageApi.delete_command(TARGET, "456")
    refute_includes command, "gsutil"
  end

  def test_cleanup_uses_exact_api_delete_operation
    command = Cloud::ObjectMove.cleanup_command(TARGET, "456")

    assert_includes command, Cloud::StorageApi.delete_command(TARGET, "456")
    refute_includes command, "gsutil"
  end

  def test_object_options_keep_leading_hyphens_in_one_argument
    target = "gs://bucket/-target?"

    assert_argument Cloud::StorageApi.upload_command("/tmp/file", target), "--object=-target?"
    assert_argument Cloud::StorageApi.stat_command(target), "--object=-target?"
    assert_argument Cloud::StorageApi.state_command(target), "--object=-target?"
    assert_argument Cloud::StorageApi.delete_command(target, "123"), "--object=-target?"
  end

  def test_uri_object_options_keep_leading_hyphens_in_one_argument
    source = "gs://bucket/-source*"
    target = "gs://bucket/-target?"

    copy = Cloud::StorageApi.copy_command(source, target, source_generation: "123", destination_generation: "0")
    assert_argument copy, "--source-object=-source*"
    assert_argument copy, "--target-object=-target?"

    move = Cloud::StorageApi.move_command(source, target, source_generation: "123")
    assert_argument move, "--source-object=-source*"
    assert_argument move, "--target-object=-target?"
  end

  private

  def assert_argument(command, argument)
    assert_includes Shellwords.split(command), argument
  end
end
