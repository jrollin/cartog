# Redis-backed cache.

require_relative '../utils/helpers'
require_relative 'base_cache'

# Redis cache implementation.
class RedisCache < BaseCache
  def initialize(host = 'localhost', port = 6379)
    super('redis')
    @store = {}
    @expiry = {}
    get_logger('cache.redis').info("RedisCache created: #{host}:#{port}")
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
      return @store[key]
    end
    @misses += 1
    nil
  end

  def set(key, value, ttl = 300)
    @store[key] = value
    @expiry[key] = Time.now.to_i + ttl
    get_logger('cache.redis').info("Redis SET #{key} (ttl=#{ttl})")
  end

  def delete(key)
    @expiry.delete(key)
    !@store.delete(key).nil?
  end

  def clear
    count = @store.size
    @store.clear
    @expiry.clear
    get_logger('cache.redis').info("Redis FLUSHDB: #{count} keys")
    count
  end

  def incr(key, amount = 1)
    current = @store[key] || 0
    new_val = current + amount
    @store[key] = new_val
    new_val
  end
end
