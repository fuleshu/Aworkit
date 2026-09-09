# Chats without a project

Every executable workflow can start with **No project** selected, including
Standard Agent and workflows with explicit file-tool nodes. Enabled tools and
configured model tiers are still required.

At first send, the trusted runtime creates `<profile>/chat-workspaces/<chat-id>`
and freezes its canonical filesystem identity in the Chat context. File tools
are confined to that folder; host shell and Python use it as their working
directory. It persists across follow-up messages and app restarts. The model
receives its working directory with an explicit indication that no saved project
was selected. No project record or project approval grant is created.

New Chats and forks each receive a separate empty folder. Forking copies the
conversation, not working files. Existing saved-project Chats keep their frozen
project binding. Legacy frozen Chats retain their original workspace authority.
Private folders are retained with local profile data; deleting a Chat does not
delete its working files automatically.
