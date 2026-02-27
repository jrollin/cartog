# Cache interface and base class.

# Base cache with stats tracking.
class BaseCache
  attr_reader :name

  def initialize(name)
    @name = name
    @hits = 0
    @misses = 0
  end

  def get(key)
    raise NotImplementedError, 'Subclass must implement get'
  end

  def set(key, value, ttl = 300)
    raise NotImplementedError, 'Subclass must implement set'
  end

  def delete(key)
    raise NotImplementedError, 'Subclass must implement delete'
  end

  def clear
    raise NotImplementedError, 'Subclass must implement clear'
  end

  def stats
    total = @hits + @misses
    rate = total > 0 ? (@hits.to_f / total * 100) : 0.0
    { backend: @name, hits: @hits, misses: @misses, hit_rate: "#{rate.round(1)}%" }
  end
end
