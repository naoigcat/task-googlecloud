module InterruptTestHelper
  def with_interrupt_after_side_effect
    release = Queue.new
    delivered = Queue.new
    interrupter = interrupt_thread(release, delivered)
    yield interrupt_trigger(release, delivered)
  ensure
    release << true
    interrupter&.join
  end

  private

  def interrupt_thread(release, delivered)
    Thread.new do
      release.pop
      Thread.main.raise(Interrupt)
      delivered << true
    end
  end

  def interrupt_trigger(release, delivered)
    lambda do
      release << true
      delivered.pop
    end
  end
end
