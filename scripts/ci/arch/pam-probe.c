/* Real pam_unix admission in a disposable Arch container; no PAM session opens. */
#include <security/pam_appl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static int converse(int count, const struct pam_message **messages,
                    struct pam_response **responses, void *password) {
    struct pam_response *reply = calloc((size_t)count, sizeof(*reply));
    if (reply == NULL) return PAM_BUF_ERR;
    for (int i = 0; i < count; ++i) {
        if (messages[i]->msg_style == PAM_PROMPT_ECHO_OFF) {
            reply[i].resp = strdup(password);
            if (reply[i].resp == NULL) {
                for (int j = 0; j < i; ++j) free(reply[j].resp);
                free(reply);
                return PAM_BUF_ERR;
            }
        } else if (messages[i]->msg_style != PAM_TEXT_INFO &&
                   messages[i]->msg_style != PAM_ERROR_MSG) {
            for (int j = 0; j < i; ++j) free(reply[j].resp);
            free(reply);
            return PAM_CONV_ERR;
        }
    }
    *responses = reply;
    return PAM_SUCCESS;
}

int main(int argc, char **argv) {
    char password[256];
    if (argc != 2 || fgets(password, sizeof(password), stdin) == NULL) return 2;
    password[strcspn(password, "\r\n")] = '\0';
    struct pam_conv conversation = { converse, password };
    pam_handle_t *handle = NULL;
    int status = pam_start("hypr-rdp", argv[1], &conversation, &handle);
    if (status == PAM_SUCCESS) status = pam_authenticate(handle, 0);
    if (status == PAM_SUCCESS) status = pam_acct_mgmt(handle, 0);
    if (handle != NULL) pam_end(handle, status);
    memset(password, 0, sizeof(password));
    puts(status == PAM_SUCCESS ? "PAM admitted" : "PAM refused");
    return status == PAM_SUCCESS ? 0 : 1;
}
