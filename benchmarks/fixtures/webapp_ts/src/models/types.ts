/** Shared type definitions. */

export enum UserRole {
    User = "user",
    Admin = "admin",
    Moderator = "moderator",
}

export enum PaymentStatus {
    Pending = "pending",
    Processing = "processing",
    Completed = "completed",
    Failed = "failed",
    Refunded = "refunded",
}

export enum EventType {
    UserRegistered = "user.registered",
    LoginSuccess = "auth.login_success",
    LoginFailed = "auth.login_failed",
    PaymentCompleted = "payment.completed",
    PaymentRefunded = "payment.refunded",
    PasswordChanged = "auth.password_changed",
}

export enum NotificationChannel {
    Email = "email",
    SMS = "sms",
    Push = "push",
    InApp = "in_app",
}
