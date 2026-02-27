package tasks

import (
    "webapp_go/pkg/logger"
    "webapp_go/internal/services/email"
)

var emailTaskLog = logger.GetLogger("tasks.email")

// EmailTask represents a background email sending task.
type EmailTask struct {
    Sender     *email.EmailSender
    Recipients []string
    Subject    string
    Body       string
    Completed  bool
}

// NewEmailTask creates a new email task.
func NewEmailTask(sender *email.EmailSender, recipients []string, subject, body string) *EmailTask {
    emailTaskLog.Info("Creating EmailTask: subject=%s, recipients=%d", subject, len(recipients))
    return &EmailTask{
        Sender:     sender,
        Recipients: recipients,
        Subject:    subject,
        Body:       body,
        Completed:  false,
    }
}

// Execute runs the email task.
func (t *EmailTask) Execute() error {
    emailTaskLog.Info("Executing email task: %s", t.Subject)
    sent, err := t.Sender.SendBulk(t.Recipients, t.Subject, t.Body)
    if err != nil {
        emailTaskLog.Error("Email task failed: %v", err)
        return err
    }
    t.Completed = true
    emailTaskLog.Info("Email task completed: %d/%d sent", sent, len(t.Recipients))
    return nil
}

// Status returns the task status.
func (t *EmailTask) Status() string {
    if t.Completed {
        return "completed"
    }
    return "pending"
}
