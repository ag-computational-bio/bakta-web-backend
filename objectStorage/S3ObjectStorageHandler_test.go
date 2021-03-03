package objectStorage

import (
	"testing"
)

func TestInitS3ObjectStorageHandler(t *testing.T) {
	handler, err := InitS3ObjectStorageHandler("foo")
	if err != nil {
		t.Fatal(err.Error())
	}

	_, err = handler.CreateDownloadLinks("foo", "baa", "test")
	if err != nil {
		t.Fatal(err.Error())
	}
}
