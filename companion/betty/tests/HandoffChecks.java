package eu.tsilva.homewire.betty;

public final class HandoffChecks {
    private static void check(boolean value){if(!value)throw new AssertionError("Send handoff contract failed");}
    public static void main(String[] args){
        Handoff handoff=new Handoff();
        check(!handoff.confirmed());check(!handoff.take(true));
        handoff.submitted();check(!handoff.ready());check(!handoff.take(true));
        check(handoff.confirmed());check(!handoff.confirmed());
        check(!handoff.take(false));check(handoff.ready());
        check(handoff.take(true));check(!handoff.take(true));
        handoff.defer();check(!handoff.take(false));check(handoff.take(true));
        handoff.submitted();handoff.abandon();check(!handoff.confirmed());check(!handoff.take(true));
        check(TelegramLink.chat("SyntheticBettyBot").equals("tg://resolve?domain=SyntheticBettyBot"));
        for(String invalid:new String[]{"bot&start=reset","bot?text=second-send","@SyntheticBettyBot",""}){
            boolean rejected=false;try{TelegramLink.chat(invalid);}catch(IllegalArgumentException e){rejected=true;}check(rejected);
        }
    }
}
