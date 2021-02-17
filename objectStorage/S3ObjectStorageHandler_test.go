package objectStorage

import (
	"testing"
)

func TestInitS3ObjectStorageHandler(t *testing.T) {
	handler := InitS3ObjectStorageHandler()
	_, err := handler.CreateDownloadLinks("foo", "baa", "test")
	if err != nil {
		t.Fatal(err.Error())
	}
}
