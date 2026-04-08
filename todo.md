debug page has no event or action queue
debug page has a breakpoint section, i want to be able to click an event or action and set a breakpoint on that one.
state view is differnt on chaos and dashboard page.
we should take a closeer look on the whole page. it seems like the tab are not bahaving as i want i need them to hold state and update also when not active.
could process_event also return and indication that state has changed or not so we know if we need to notify.
OKay, we need to look more at the ui and how it get data. i started the dev stack and it containes nothing. the dashboard page just shows "Connecting to Windlass…". it might be because there are no new events?
event loop in shell awaits all actions before processing the next event.

add donation to milionaires vault automation to spec

copilot --resume=b7c59de2-a45f-4dee-b671-833558330801
