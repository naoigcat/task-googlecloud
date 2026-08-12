require "fiddle"

module AtomicRename
  AT_FDCWD = -100
  LINUX_RENAME_NOREPLACE = 1
  DARWIN_RENAME_EXCL = 4
  LINUX_ARGUMENT_TYPES = [
    Fiddle::TYPE_INT,
    Fiddle::TYPE_VOIDP,
    Fiddle::TYPE_INT,
    Fiddle::TYPE_VOIDP,
    Fiddle::TYPE_UINT,
  ].freeze
  DARWIN_ARGUMENT_TYPES = [Fiddle::TYPE_VOIDP, Fiddle::TYPE_VOIDP, Fiddle::TYPE_UINT].freeze
  private_constant(
    :AT_FDCWD,
    :LINUX_RENAME_NOREPLACE,
    :DARWIN_RENAME_EXCL,
    :LINUX_ARGUMENT_TYPES,
    :DARWIN_ARGUMENT_TYPES,
  )

  module_function

  def rename(source, target)
    status = rename_function.call(*rename_arguments(source, target))
    return if status.zero?

    raise SystemCallError.new("rename", Fiddle.last_error)
  end

  def rename_function
    @rename_function ||= platform_rename_function
  end

  def platform_rename_function
    return linux_rename_function if linux?
    return darwin_rename_function if darwin?

    raise NotImplementedError, "Atomic no-replace rename is not supported on #{RUBY_PLATFORM}"
  end

  def linux_rename_function
    Fiddle::Function.new(Fiddle::Handle::DEFAULT["renameat2"], LINUX_ARGUMENT_TYPES, Fiddle::TYPE_INT)
  end

  def darwin_rename_function
    Fiddle::Function.new(Fiddle::Handle::DEFAULT["renamex_np"], DARWIN_ARGUMENT_TYPES, Fiddle::TYPE_INT)
  end

  def linux?
    RUBY_PLATFORM.match?("linux")
  end

  def darwin?
    RUBY_PLATFORM.match?("darwin")
  end

  def rename_arguments(source, target)
    return [AT_FDCWD, source, AT_FDCWD, target, LINUX_RENAME_NOREPLACE] if linux?

    [source, target, DARWIN_RENAME_EXCL]
  end
end
