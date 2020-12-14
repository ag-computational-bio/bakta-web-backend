package objectStorage

import (
	"log"
	"time"

	"github.com/aws/aws-sdk-go/aws"
	"github.com/aws/aws-sdk-go/aws/session"
	"github.com/aws/aws-sdk-go/service/s3"
)

type S3ObjectStorageHandler struct {
	S3Client *s3.S3
}

func InitS3ObjectStorageHandler() *S3ObjectStorageHandler {
	s3Config := &aws.Config{
		Endpoint:         aws.String("s3.computational.bio.uni-giessen.de"),
		Region:           aws.String("RegionOne"),
		S3ForcePathStyle: aws.Bool(true),
	}

	sess := session.Must(session.NewSession(s3Config))
	s3Client := s3.New(sess)

	objectHandler := S3ObjectStorageHandler{
		S3Client: s3Client,
	}

	return &objectHandler
}

func (handler *S3ObjectStorageHandler) CreateUploadLink(bucket string, key string) (string, error) {
	req, _ := handler.S3Client.PutObjectRequest(&s3.PutObjectInput{
		Bucket: aws.String(bucket),
		Key:    aws.String(key),
	})

	uploadURL, err := req.Presign(60 * time.Minute)
	if err != nil {
		log.Println(err.Error())
		return "", err
	}

	return uploadURL, nil
}

func (handler *S3ObjectStorageHandler) CreateDownloadLink(bucket string, key string) (string, error) {
	req, _ := handler.S3Client.GetObjectRequest(&s3.GetObjectInput{
		Bucket: aws.String(bucket),
		Key:    aws.String(key),
	})

	downloadURL, err := req.Presign(60 * time.Minute)
	if err != nil {
		log.Println(err.Error())
		return "", err
	}

	return downloadURL, nil
}
