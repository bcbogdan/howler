#include "howler_application.h"
int main(void) {
    HowlerNoteFolder *folder = 0;
    HowlerApplicationSession *session = 0;
    int32_t (*create_session)(HowlerApplicationSession **) = howler_session_create;
    return folder == 0 && session == 0 && create_session != 0 ? 0 : 1;
}
