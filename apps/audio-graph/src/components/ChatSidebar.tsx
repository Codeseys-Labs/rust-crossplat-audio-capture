import { useState, useRef, useEffect } from "react";
import { useAudioGraphStore } from "../store";
import type { ChatMessage } from "../types";

function ChatSidebar() {
    const chatMessages = useAudioGraphStore((s) => s.chatMessages);
    const isChatLoading = useAudioGraphStore((s) => s.isChatLoading);
    const sendChatMessage = useAudioGraphStore((s) => s.sendChatMessage);
    const clearChatHistory = useAudioGraphStore((s) => s.clearChatHistory);
    const graphSnapshot = useAudioGraphStore((s) => s.graphSnapshot);

    const [input, setInput] = useState("");
    const messagesEndRef = useRef<HTMLDivElement>(null);
    const inputRef = useRef<HTMLInputElement>(null);

    // Auto-scroll to bottom on new messages
    useEffect(() => {
        messagesEndRef.current?.scrollIntoView({ behavior: "smooth" });
    }, [chatMessages, isChatLoading]);

    const handleSend = async () => {
        const trimmed = input.trim();
        if (!trimmed || isChatLoading) return;
        setInput("");
        await sendChatMessage(trimmed);
        inputRef.current?.focus();
    };

    const handleKeyDown = (e: React.KeyboardEvent) => {
        if (e.key === "Enter" && !e.shiftKey) {
            e.preventDefault();
            handleSend();
        }
    };

    return (
        <div className="chat-sidebar">
            <div className="chat-sidebar__header">
                <h3 className="chat-sidebar__title">💬 Chat</h3>
                <div className="chat-sidebar__actions">
                    <span className="chat-sidebar__context-badge" title="Graph context available">
                        {graphSnapshot.stats.total_nodes} entities
                    </span>
                    {chatMessages.length > 0 && (
                        <button
                            className="chat-sidebar__clear-btn"
                            onClick={clearChatHistory}
                            title="Clear chat history"
                        >
                            🗑️
                        </button>
                    )}
                </div>
            </div>

            <div className="chat-sidebar__messages">
                {chatMessages.length === 0 && !isChatLoading && (
                    <div className="chat-sidebar__empty">
                        <p>Ask questions about the conversation and knowledge graph.</p>
                        <p className="chat-sidebar__hint">
                            Try: "What entities have been mentioned?" or "Summarize the conversation so far"
                        </p>
                    </div>
                )}

                {chatMessages.map((msg: ChatMessage, idx: number) => (
                    <div
                        key={`${msg.role}-${idx}`}
                        className={`chat-sidebar__message chat-sidebar__message--${msg.role}`}
                    >
                        <div className="chat-sidebar__message-role">
                            {msg.role === "user" ? "You" : "Assistant"}
                        </div>
                        <div className="chat-sidebar__message-content">
                            {msg.content}
                        </div>
                    </div>
                ))}

                {isChatLoading && (
                    <div className="chat-sidebar__message chat-sidebar__message--assistant">
                        <div className="chat-sidebar__message-role">Assistant</div>
                        <div className="chat-sidebar__thinking">
                            <span className="chat-sidebar__dot"></span>
                            <span className="chat-sidebar__dot"></span>
                            <span className="chat-sidebar__dot"></span>
                        </div>
                    </div>
                )}

                <div ref={messagesEndRef} />
            </div>

            <div className="chat-sidebar__input-area">
                <input
                    ref={inputRef}
                    type="text"
                    className="chat-sidebar__input"
                    placeholder="Ask about the conversation..."
                    value={input}
                    onChange={(e) => setInput(e.target.value)}
                    onKeyDown={handleKeyDown}
                    disabled={isChatLoading}
                />
                <button
                    className="chat-sidebar__send-btn"
                    onClick={handleSend}
                    disabled={!input.trim() || isChatLoading}
                    title="Send message"
                >
                    ➤
                </button>
            </div>
        </div>
    );
}

export default ChatSidebar;
