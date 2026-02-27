# Cacheable module for services with built-in caching.

require_relative '../utils/helpers'

# Mixin providing cache get/set/invalidate to any service.
module Cacheable
  def cache_get(key)
    @cache_store ||= {}
    entry = @cache_store[key]
    if entry && Time.now.to_i < entry[:expiry]
      get_logger('cacheable').info("Cache hit: #{key}")
      return entry[:value]
    end
    @cache_store.delete(key) if entry
    get_logger('cacheable').info("Cache miss: #{key}")
    nil
  end

  def cache_set(key, value, ttl = 300)
    @cache_store ||= {}
    @cache_store[key] = { value: value, expiry: Time.now.to_i + ttl }
    get_logger('cacheable').info("Cache set: #{key} (ttl=#{ttl}s)")
  end

  def cache_invalidate(key)
    @cache_store ||= {}
    !@cache_store.delete(key).nil?
  end

  def cache_clear
    @cache_store ||= {}
    count = @cache_store.size
    @cache_store.clear
    get_logger('cacheable').info("Cache cleared: #{count} entries")
    count
  end
end
