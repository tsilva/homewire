package eu.tsilva.homewire.betty;

/** A send tap alone cannot open Telegram; a confirmed delivery can open it once. */
final class Handoff {
    private boolean waiting, ready;
    void submitted() { waiting=true;ready=false; }
    boolean confirmed() { if(!waiting)return false;waiting=false;ready=true;return true; }
    boolean ready() { return ready; }
    boolean take(boolean foreground) { if(!ready||!foreground)return false;ready=false;return true; }
    void defer() { ready=true; }
    void abandon() { waiting=false;ready=false; }
}
