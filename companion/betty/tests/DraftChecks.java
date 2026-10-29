package eu.tsilva.homewire.betty;

public final class DraftChecks {
    private static void check(boolean ok) { if (!ok) throw new AssertionError(); }
    public static void main(String[] args) {
        Draft draft = new Draft();
        check(!draft.send()); check(!draft.stop());
        check(draft.start()); check(!draft.start()); check(!draft.send());
        check(draft.stop()); check(draft.state == Draft.State.REVIEW);
        check(draft.send()); check(!draft.send()); check(!draft.cancel()); check(!draft.start());
        draft.state = Draft.State.UNCERTAIN;
        check(!draft.send()); check(!draft.cancel()); check(!draft.start());
        draft.state = Draft.State.TEXT; check(draft.send());
        draft.state = Draft.State.RECORDING; check(draft.cancel()); check(draft.state == Draft.State.IDLE);
        System.out.println("Capture/review/send state checks passed");
    }
}
