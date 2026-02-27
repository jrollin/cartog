package notification

import (
    "fmt"

    "webapp_go/pkg/logger"
)

var notifLog = logger.GetLogger("services.notification")

// NotificationType represents the type of notification.
type NotificationType int

const (
    NotifEmail NotificationType = iota
    NotifSMS
    NotifPush
    NotifInApp
)

// Notification represents a message to be sent to a user.
type Notification struct {
    ID      string
    UserID  string
    Type    NotificationType
    Title   string
    Body    string
    Sent    bool
}

// NotificationManager handles sending notifications through various channels.
type NotificationManager struct {
    Queue    []*Notification
    Handlers map[NotificationType]func(*Notification) error
}

// NewNotificationManager creates a manager with default handlers.
func NewNotificationManager() *NotificationManager {
    notifLog.Info("Creating NotificationManager")
    mgr := &NotificationManager{
        Queue:    make([]*Notification, 0),
        Handlers: make(map[NotificationType]func(*Notification) error),
    }
    mgr.Handlers[NotifEmail] = func(n *Notification) error {
        notifLog.Info("Sending email notification to user: %s", n.UserID)
        return nil
    }
    mgr.Handlers[NotifSMS] = func(n *Notification) error {
        notifLog.Info("Sending SMS notification to user: %s", n.UserID)
        return nil
    }
    mgr.Handlers[NotifPush] = func(n *Notification) error {
        notifLog.Info("Sending push notification to user: %s", n.UserID)
        return nil
    }
    mgr.Handlers[NotifInApp] = func(n *Notification) error {
        notifLog.Info("Sending in-app notification to user: %s", n.UserID)
        return nil
    }
    return mgr
}

// Send dispatches a notification through the appropriate channel.
func (m *NotificationManager) Send(notif *Notification) error {
    notifLog.Info("Sending notification: type=%d, user=%s", notif.Type, notif.UserID)
    handler, ok := m.Handlers[notif.Type]
    if !ok {
        notifLog.Error("No handler for notification type: %d", notif.Type)
        return fmt.Errorf("unsupported notification type: %d", notif.Type)
    }
    if err := handler(notif); err != nil {
        notifLog.Error("Failed to send notification: %v", err)
        return err
    }
    notif.Sent = true
    notifLog.Info("Notification sent successfully: %s", notif.ID)
    return nil
}

// Enqueue adds a notification to the processing queue.
func (m *NotificationManager) Enqueue(notif *Notification) {
    notifLog.Info("Enqueuing notification for user: %s", notif.UserID)
    m.Queue = append(m.Queue, notif)
}

// ProcessQueue sends all queued notifications.
func (m *NotificationManager) ProcessQueue() int {
    notifLog.Info("Processing notification queue (%d items)", len(m.Queue))
    sent := 0
    for _, notif := range m.Queue {
        if err := m.Send(notif); err == nil {
            sent++
        }
    }
    m.Queue = m.Queue[:0]
    notifLog.Info("Processed queue: %d sent", sent)
    return sent
}
