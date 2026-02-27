package events

import (
    "fmt"

    "webapp_go/pkg/logger"
)

var dispLog = logger.GetLogger("events.dispatcher")

// Event represents an application event.
type Event struct {
    Name    string
    Payload map[string]interface{}
    Source  string
}

// NewEvent creates a new event with the given name and payload.
func NewEvent(name, source string, payload map[string]interface{}) *Event {
    dispLog.Debug("Creating event: %s from %s", name, source)
    return &Event{
        Name:    name,
        Payload: payload,
        Source:  source,
    }
}

// EventHandler is a function that handles an event.
type EventHandler func(*Event) error

// EventDispatcher manages event listeners and dispatching.
type EventDispatcher struct {
    listeners map[string][]EventHandler
}

// NewEventDispatcher creates a new dispatcher.
func NewEventDispatcher() *EventDispatcher {
    dispLog.Info("Creating EventDispatcher")
    return &EventDispatcher{
        listeners: make(map[string][]EventHandler),
    }
}

// On registers an event handler for the given event name.
func (d *EventDispatcher) On(eventName string, handler EventHandler) {
    dispLog.Info("Registering handler for event: %s", eventName)
    d.listeners[eventName] = append(d.listeners[eventName], handler)
}

// Dispatch triggers all handlers registered for the event.
func (d *EventDispatcher) Dispatch(event *Event) error {
    dispLog.Info("Dispatching event: %s", event.Name)
    handlers, ok := d.listeners[event.Name]
    if !ok {
        dispLog.Warn("No handlers for event: %s", event.Name)
        return nil
    }
    for i, handler := range handlers {
        dispLog.Debug("Calling handler %d for event: %s", i, event.Name)
        if err := handler(event); err != nil {
            dispLog.Error("Handler %d failed for event %s: %v", i, event.Name, err)
            return fmt.Errorf("handler failed: %w", err)
        }
    }
    dispLog.Info("Event dispatched: %s (%d handlers)", event.Name, len(handlers))
    return nil
}

// RemoveAll removes all handlers for an event.
func (d *EventDispatcher) RemoveAll(eventName string) {
    dispLog.Info("Removing all handlers for event: %s", eventName)
    delete(d.listeners, eventName)
}

// HasListeners checks if an event has any handlers.
func (d *EventDispatcher) HasListeners(eventName string) bool {
    handlers, ok := d.listeners[eventName]
    return ok && len(handlers) > 0
}
