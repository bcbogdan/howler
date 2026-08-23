Howleer is mean to be a notes app that can do the following:

- Write notes in markdown format
- Save notes locally and then sync them over other devices
- Have contextual information in notes like github/local repo references, todo items, dates, locations, meeting links, etc
- Have a common core engine that can be imported / linked into different apps/editors (desktop/mobile/cli)
- Have a todo management mechanism (todo items can grouped and handled together, todos can have deadlines, reminders can be triggered based on those things)
- Have a plugin system that can do stuff like: sync with a calendar and create a new note for that meeting.
- Have a mac desktop app that looks and behaves in a similar way to raycast notes (https://www.raycast.com/core-features/notes):
  - The editor can be pinned over other apps
  - You only see the editor without any other custom elements
  - cmd+p triggers a command palette where you can search for notes
  - cmd+n creates a new note

Your task is to understand the specs and createa more comprehensive spec document. You can ask me questions if you need to clarify anything.
We don't need to go in depth with each spec. The main aim of the app is:

- to be a simple note taking / editor engine that can be used inside different contextes. Something like how libghostty is for the terminal world (https://github.com/ghostty-org/ghostty)
- to create the mac app in order to enable my flow

Based on that devise an architectural plan that will look to determine the tech stack, models, data flows, which will be at the base of our app.
