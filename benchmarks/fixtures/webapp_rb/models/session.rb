# Session model.

class Session
  attr_reader :token, :user, :user_id, :expires_at

  def initialize(user:, token:, expires_at:)
    @user = user
    @user_id = user.id
    @token = token
    @expires_at = expires_at
  end

  def expired?
    Time.now.utc > @expires_at
  end

  def delete
    # Remove session from storage
  end

  def self.create(user:, token:, expires_in:)
    expires_at = Time.now.utc + expires_in
    new(user: user, token: token, expires_at: expires_at)
  end

  def self.find_by_token(token)
    # Look up session by token
  end

  def self.find_all_by_user(_user)
    # Find all sessions for a user
    []
  end
end
