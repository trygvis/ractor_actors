# Chat Server

This server implements a party-line chat where everyone gets all messages from everyone. You can connect to the
party-line on port 9999.

It depends on the net/tcp actors to do the low-level networking.

The ChatServer spawns a ChatListener. The ChatListener will in turn spawn the Listener, and give it callback that is
called on every new connection. When a new connection is created it will spawn a ChatSession.

The ChatSession will spawn a Session with a LineReader actor. The Session will spawn the LineReader as its internal
reader actor and spawn a default writer. The session is by default linked to the reader and writer, but it is also
linked to the ChatSession. The sessions supervision strategy is to terminate all of its children on any kind of failure,
and then terminate itself. This means that the ChatSession can stop from either an explicit message or panic, and the
entire session will be torn down. Similarly, if the client connection drops the session will also be torn down.

On a new connection, the chat session will send a prompt for the user's nick. After the nick is received, it will
register itself with the ChatServer. The ChatServer will then:

1) Monitor the actor
2) Broadcast a Join message to all registered users.

When an actor dies, it will be removed from the ChatServers state, and a Part will be broadcast to all users. When the
ChatSession receives this message, it will send a message to the connected client.

![Actor overview](chat_server.drawio.svg)
