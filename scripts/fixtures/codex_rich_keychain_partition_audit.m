#import <Foundation/Foundation.h>
#import <Security/Security.h>

static void fail(NSString *message, OSStatus status) {
    fprintf(stderr, "%s: %d\n", message.UTF8String, (int)status);
    exit(1);
}

int main(int argc, const char *argv[]) {
    @autoreleasepool {
        if (argc != 3) {
            fprintf(stderr, "usage: %s SERVICE ACCOUNT\n", argv[0]);
            return 2;
        }
        OSStatus interaction = SecKeychainSetUserInteractionAllowed(false);
        if (interaction != errSecSuccess) fail(@"could not disable Keychain UI", interaction);

        NSString *service = [NSString stringWithUTF8String:argv[1]];
        NSString *account = [NSString stringWithUTF8String:argv[2]];
        NSDictionary *query = @{
            (__bridge id)kSecClass: (__bridge id)kSecClassGenericPassword,
            (__bridge id)kSecAttrService: service,
            (__bridge id)kSecAttrAccount: account,
            (__bridge id)kSecMatchLimit: (__bridge id)kSecMatchLimitOne,
            (__bridge id)kSecReturnRef: @YES,
        };
        CFTypeRef found = NULL;
        OSStatus status = SecItemCopyMatching((__bridge CFDictionaryRef)query, &found);
        if (status != errSecSuccess) fail(@"could not find the exact synthetic Keychain item", status);

        SecAccessRef access = NULL;
        status = SecKeychainItemCopyAccess((__bridge SecKeychainItemRef)found, &access);
        CFRelease(found);
        if (status != errSecSuccess) fail(@"could not read synthetic Keychain item access metadata", status);

        CFArrayRef acls = NULL;
        status = SecAccessCopyACLList(access, &acls);
        CFRelease(access);
        if (status != errSecSuccess) fail(@"could not read synthetic Keychain ACLs", status);

        NSArray *partitions = nil;
        for (id value in (__bridge NSArray *)acls) {
            SecACLRef acl = (__bridge SecACLRef)value;
            CFArrayRef authorizations = SecACLCopyAuthorizations(acl);
            BOOL isPartition = [(__bridge NSArray *)authorizations
                containsObject:(__bridge id)kSecACLAuthorizationPartitionID];
            if (authorizations) CFRelease(authorizations);
            if (!isPartition) continue;

            CFArrayRef applications = NULL;
            CFStringRef description = NULL;
            SecKeychainPromptSelector flags = 0;
            status = SecACLCopyContents(acl, &applications, &description, &flags);
            if (applications) CFRelease(applications);
            if (status != errSecSuccess || !description) {
                if (description) CFRelease(description);
                CFRelease(acls);
                fail(@"could not decode synthetic Keychain partition metadata", status);
            }

            NSString *encoded = (__bridge NSString *)description;
            NSMutableData *plistData = [NSMutableData data];
            for (NSUInteger offset = 0; offset + 1 < encoded.length; offset += 2) {
                unsigned value = 0;
                NSScanner *scanner = [NSScanner scannerWithString:[encoded substringWithRange:NSMakeRange(offset, 2)]];
                if (![scanner scanHexInt:&value]) {
                    CFRelease(acls);
                    CFRelease(description);
                    fprintf(stderr, "synthetic Keychain partition metadata was not hexadecimal\n");
                    return 1;
                }
                uint8_t byte = (uint8_t)value;
                [plistData appendBytes:&byte length:1];
            }
            NSError *error = nil;
            NSDictionary *payload = [NSPropertyListSerialization propertyListWithData:plistData
                options:NSPropertyListImmutable format:nil error:&error];
            CFRelease(description);
            if (![payload isKindOfClass:NSDictionary.class]
                || ![payload[@"Partitions"] isKindOfClass:NSArray.class]) {
                CFRelease(acls);
                fprintf(stderr, "could not parse synthetic Keychain partition metadata: %s\n", error.localizedDescription.UTF8String);
                return 1;
            }
            partitions = payload[@"Partitions"];
            break;
        }
        CFRelease(acls);
        if (!partitions) {
            fprintf(stderr, "synthetic Keychain item has no partition ACL\n");
            return 1;
        }
        NSData *output = [NSJSONSerialization dataWithJSONObject:partitions options:0 error:nil];
        fwrite(output.bytes, 1, output.length, stdout);
        fputc('\n', stdout);
    }
    return 0;
}
