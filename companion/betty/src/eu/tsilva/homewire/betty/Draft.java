package eu.tsilva.homewire.betty;

/** Explicit capture/review/send transitions; opening the app never sends. */
final class Draft {
    enum State { IDLE, RECORDING, REVIEW, TEXT, SENDING, UNCERTAIN }
    State state = State.IDLE;
    boolean start() {
        if (state != State.IDLE) return false;
        state = State.RECORDING; return true;
    }
    boolean stop() {
        if (state != State.RECORDING) return false;
        state = State.REVIEW; return true;
    }
    boolean send() {
        if (state != State.REVIEW && state != State.TEXT) return false;
        state = State.SENDING; return true;
    }
    boolean cancel() {
        if (state == State.SENDING || state == State.UNCERTAIN) return false;
        state = State.IDLE; return true;
    }
}
