# In-memory LRU cache.

require_relative '../utils/helpers'
require_relative 'base_cache'

# LRU memory cache.
class MemoryCache < BaseCache
  def initialize(max_size = 1000)
    super('memory')
    @store = {}
    @expiry = {}
    @max_size = max_size
    get_logger('cache.memory').info("MemoryCache created: max_size=#{max_size}")
  end

  def get(key)
    if @store.key?(key)
      exp = @expiry[key] || Float::INFINITY
      if Time.now.to_i > exp
        @store.delete(key)
        @expiry.delete(key)
        @misses += 1
        return nil
      end
      @hits += 1
      # Move to end for LRU
      val = @store.delete(key)
      @store[key] = val
      return val
    end
    @misses += 1
    nil
  end

  def set(key, value, ttl = 300)
    if @store.key?(key)
      @store.delete(key)
    elsif @store.size >= @max_size
      first_key = @store.keys.first
      if first_key
        @store.delete(first_key)
        @expiry.delete(first_key)
        get_logger('cache.memory').info("LRU evicted: #{first_key}")
      end
    end
    @store[key] = value
    @expiry[key] = Time.now.to_i + ttl
  end

  def delete(key)
    @expiry.delete(key)
    !@store.delete(key).nil?
  end

  def clear
    count = @store.size
    @store.clear
    @expiry.clear
    count
  end

  def size
    @store.size
  end
end
