#include "howler_application.h"
int main(void) {
    HowlerNoteFolder *folder = 0;
    HowlerApplicationSession *session = 0;
    int32_t (*create_session)(HowlerApplicationSession **) = howler_session_create;
    int32_t (*capabilities)(HowlerApplicationSession *, char **, char **) = howler_session_capabilities_json;
    int32_t (*apply_selection)(HowlerApplicationSession *, const char *, char **, char **) = howler_session_apply_selection_json;
    return folder == 0 && session == 0 && create_session != 0 && capabilities != 0 && apply_selection != 0 ? 0 : 1;
}
